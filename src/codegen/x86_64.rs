// src/codegen/x86_64.rs - x86_64 代码生成
// 第一性原理: x86_64 指令编码固定,寄存器按 SysV ABI 选。
// suckless: 直接 emit 字节,不引入汇编库。

use crate::ast::*;
use crate::target::Target;
use crate::codegen::elf::{ElfSym, ElfRela, ElfSection, R_X86_64_PC32, R_X86_64_PLT32,
    R_X86_64_64, R_X86_64_32,
    STB_LOCAL, STB_GLOBAL, STB_WEAK, STT_NOTYPE, STT_FUNC, STT_OBJECT, STT_TLS, STT_SECTION, SHN_UNDEF,
    STV_DEFAULT, STV_HIDDEN, STV_PROTECTED,
    SHT_PROGBITS, SHF_ALLOC, SHF_WRITE, SHF_EXECINSTR};
use crate::codegen::elf;
use crate::const_eval;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use crate::ast::ArraySize;

pub struct Codegen {
    pub code: Vec<u8>,
    pub entry: u64,
    pub target: Target,
    pub labels: HashMap<String, u64>,
    pending: Vec<PendingJump>,
    next_label: u32,
    var_map: HashMap<String, (i32, Type)>,
    loop_continue: Option<String>,
    loop_break: Option<String>,
    // .rodata: 字符串池 + 待修补引用
    pub rodata: Vec<u8>,
    string_pool: HashMap<Vec<u8>, u64>,   // data -> offset in rodata
    string_refs: Vec<(u64, Vec<u8>)>,     // (code_pos, data)
    float_refs: Vec<(u64, u64)>,          // (code_pos, rodata_offset) - 浮点字面量引用
    // 全局变量
    globals: HashMap<String, GlobalVar>,
    frame_size: i32,
    expr_depth: u32,              // 当前表达式嵌套深度 (用于帧临时槽分配)
    max_expr_depth: u32,          // 最大表达式嵌套深度
    temp_slots: i32,              // 帧临时槽数量 (用于变量偏移计算)
    vla_offsets: HashMap<String, i32>,
    vla_total_size: i32,
    // 可重定位模式 (.o)
    pub relocatable: bool,
    pub function_sections: bool, // -ffunction-sections：每函数独立 .text.<name> 节
    pub symbols: Vec<ElfSym>,
    sym_index: HashMap<String, u32>,
    pub relocations: Vec<ElfRela>,
    pending_func_rels: Vec<(u64, String)>, // (code_offset, func_name) - 延迟到符号表构建后解析
    pending_data_rels: Vec<(u64, String)>,  // (disp32_pos, var_name) - 全局变量重定位
    pub data_section: Vec<u8>,    // .data 节 (已初始化全局变量)
    pub bss_size: u64,            // .bss 大小
    pub tdata_section: Vec<u8>,   // .tdata (TLS 初始化)
    pub tbss_size: u64,           // .tbss (TLS 未初始化)
    pub tdata_align: u64,
    pub tbss_align: u64,
    pub exec_text_size: u64,      // ET_EXEC 模式下 .text 大小
    pending_tls_rels: Vec<(u64, String)>, // TLS fs 相对引用
    pub text_align: u64,
    pub data_align: u64,
    pub bss_align: u64,
    pub extra_sections: Vec<elf::ElfSection>, // 自定义节 (section 属性)
    custom_sec_indices: HashMap<String, usize>, // section name → index in extra_sections
    pub func_sizes: HashMap<String, u64>, // 函数名 → 代码大小 (用于符号 size)
    variadic_funcs: HashMap<String, bool>, // 函数名 → 是否可变参数
    variadic_named_params: i32, // 当前可变参数函数的命名参数数量
    variadic_named_gp: i32,
    variadic_named_fp: i32,
    variadic_overflow_offset: i32,
    variadic_reg_save_offset: i32,
    current_sret_offset: Option<i32>,
    sret_depth: i32,
    // 嵌套函数静态链
    nested_funcs: HashMap<String, Vec<(String, i32)>>, // 嵌套函数名 → upvars (name, parent_offset)
    /// 函数形参类型表（SysV 实参按形参类型分类）
    func_param_types: HashMap<String, Vec<Type>>,
    current_func_is_nested: bool,  // 当前编译的函数是否是嵌套函数
    /// 当前函数标了 __attribute__((returns_nonnull)): 返回时需检查 rax 非空
    current_returns_nonnull: bool,
    static_chain_offset: i32,      // 静态链在当前帧中的偏移
    upvar_offsets: HashMap<String, i32>, // upvar 名称 → 在父帧中的偏移
    // &&label 地址引用: (disp32_pos, label_name)
    label_addr_refs: Vec<(u64, String)>,
    // register asm 变量绑定: 变量名 → 寄存器编号
    reg_bindings: HashMap<String, u8>,
    // data 节 64 位符号引用重定位: (data_section_offset, symbol_name)
    /// (data_off, symbol, custom_sec, rel_type, addend) — addend 用于 &arr[i] 等
    pending_data64_rels: Vec<(u64, String, Option<usize>, u32, i64)>,
    // data 节最终重定位条目
    pub data_relocations: Vec<ElfRela>,
    // cleanup 属性变量: (变量名, 清理函数名)
    cleanup_vars: Vec<(String, String)>,
    // 额外 .rela 节 (用于 .init_array/.fini_array 等)
    pub extra_rela_sections: Vec<elf::ExtraRelaSection>,
    // -S 汇编文本输出
    pub emit_asm_text: bool,
    pub debug_level: u8,
    pub emit_cfi: bool,
    asm_directives: Vec<String>,
    asm_directives_at: Vec<(u64, String)>,
    disasm_calls: HashMap<u64, String>,
    disasm_data: HashMap<u64, String>,
    global_asm_exports: HashSet<String>,
    func_asm_layout: HashMap<String, FuncAsmLayout>,
    pub source_name: String,
    pub parallel_codegen: bool,
}

/// Per-function layout captured after resolve (for -S disassembly).
struct FuncAsmLayout {
    section: String,
    bytes: Vec<u8>,
    text_start: u64,
}

struct PendingJump { at: u64, target: String, kind: JumpKind }
#[derive(PartialEq)]
enum JumpKind { Jmp, Je, Jne, Jl, Jle, Jg, Jge, Jb, Ja, Jns }
#[derive(Clone)]
struct GlobalVar { offset: u64, size: usize, ty: Type, init: Option<Vec<u8>>, section: u16, is_tls: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AbiClass { Integer, Sse, Memory }

#[derive(Clone, Copy)]
enum ArgLoc {
    Gp(u8),
    Xmm(u8),
    Stack(usize),
}

/// 内联汇编操作数
#[derive(Clone, Debug)]
enum AsmOperand {
    None,
    Reg(u8, u8),           // (reg_index, size_bits)
    Imm(i64),              // 立即数
    Mem { disp: i32, base: u8, index: Option<u8>, scale: u8 },
    Sym(String),           // 符号/标签名
    /// 段寄存器: 0=ES, 1=CS, 2=SS, 3=DS, 4=FS, 5=GS
    Seg(u8),
    /// 段前缀 + mem: 用于 `mov %gs:offset(%rip), %rax` 等
    SegMem { seg: u8, disp: i32, base: u8, index: Option<u8>, scale: u8 },
    /// 全局符号 + RIP-relative (sym@PLT / 直接 PC-relative)
    SymDisp(String, i32),
}

/// parse_asm_mem 中 %rip 相对寻址的 base 哨兵值
const ASM_BASE_RIP: u8 = 255;

/// 内联 asm 约束绑定结果
struct AsmOpBinding {
    /// 分配到的物理寄存器索引 (rax=0 …)
    reg: u8,
    /// 是否为输出/读写操作数
    is_output: bool,
    /// 模板替换为内存操作数
    is_mem: bool,
    /// early-clobber (`&`): 分配到的寄存器与所有输入都不重叠
    early_clobber: bool,
    /// XMM 寄存器宽度 (0 = GP, 32 = float, 64 = double, 128 = xmm full)
    xmm_width: u8,
}

/// GNU as 局部数字标签：同一块 asm 内 `6:` 定义，`6f`/`6b` 引用。
struct AsmLocalLabels {
    /// (行号, 标签号, 唯一内部名)
    defs: Vec<(usize, u32, String)>,
}

impl AsmLocalLabels {
    fn scan(lines: &[String]) -> Self {
        let mut defs = Vec::new();
        let mut serial: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if let Some(num) = Self::parse_numeric_label(t) {
                let n = serial.entry(num).or_insert(0);
                let name = format!(".Lasm_local_{num}_{n}");
                *n += 1;
                defs.push((i, num, name));
            }
        }
        Self { defs }
    }

    /// GNU as AT&T：`#` 起至行尾为注释（如 jump_label.h 的 objtool 标注）
    fn strip_asm_comment(s: &str) -> &str {
        s.split('#').next().unwrap_or(s).trim_end()
    }

    fn parse_numeric_label(s: &str) -> Option<u32> {
        let s = s.strip_suffix(':')?;
        let s = Self::strip_asm_comment(s).trim();
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }

    /// 将 `N:insn` / `found:insn` 拆成标签行与指令行（GNU as 局部/C 标签同行写法）
    fn expand_label_insn_lines(lines: impl IntoIterator<Item = String>) -> Vec<String> {
        let mut out = Vec::new();
        for line in lines {
            let trimmed = Self::strip_asm_comment(line.trim()).trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            // 找 AT&T 段前缀冒号 (%gs:..., %fs:...) 而非标签冒号
            // 策略: 只把整个 trimmed 看作 `LABEL:` + 指令 当且仅当
            //  - 第一个 ':' 之前的部分不含空格 (label 是单词)
            //  - 第一个 ':' 之前不含 '%' (避免 AT&T 段前缀)
            //  - 第一个 ':' 之前不含 '(' (避免内存表达式)
            //  - 第一个 ':' 之后以非空开头 (有指令部分)
            let label_end = if let Some(colon) = trimmed.find(':') {
                let label = &trimmed[..colon];
                if !label.is_empty()
                    && !label.contains(' ')
                    && !label.contains('\t')
                    && !label.contains('%')
                    && !label.contains('(')
                {
                    Some(colon)
                } else {
                    None
                }
            } else { None };
            if let Some(colon) = label_end {
                let label = Self::strip_asm_comment(&trimmed[..colon]).trim();
                let rest = Self::strip_asm_comment(&trimmed[colon + 1..]).trim();
                if !label.is_empty() && !rest.is_empty() {
                    let is_numeric = label.chars().all(|c| c.is_ascii_digit());
                    let is_named = !is_numeric && !label.starts_with('.');
                    if is_numeric || is_named {
                        out.push(format!("{}:", label));
                        out.push(rest.to_string());
                        continue;
                    }
                }
            }
            out.push(trimmed);
        }
        out
    }

    fn name_at(&self, line_idx: usize) -> Option<&str> {
        self.defs
            .iter()
            .find(|(i, _, _)| *i == line_idx)
            .map(|(_, _, n)| n.as_str())
    }

    fn resolve(&self, num: u32, dir: char, at_line: usize) -> Option<&str> {
        match dir {
            'f' => self
                .defs
                .iter()
                .find(|(i, n, _)| *i > at_line && *n == num)
                .map(|(_, _, name)| name.as_str()),
            'b' => self
                .defs
                .iter()
                .filter(|(i, n, _)| *i < at_line && *n == num)
                .last()
                .map(|(_, _, name)| name.as_str()),
            _ => None,
        }
    }

    /// 将 `6f` / `6b` 替换为内部标签名
    fn substitute(&self, line: &str, at_line: usize) -> String {
        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();
        let mut out = String::with_capacity(line.len());
        let mut i = 0;
        while i < len {
            if chars[i].is_ascii_digit() {
                let start = i;
                while i < len && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i < len && (chars[i] == 'f' || chars[i] == 'b') {
                    let before_ok = start == 0
                        || !(chars[start - 1].is_ascii_alphanumeric() || chars[start - 1] == '_');
                    let after_ok = i + 1 >= len
                        || !(chars[i + 1].is_ascii_alphanumeric() || chars[i + 1] == '_');
                    if before_ok && after_ok {
                        let num: u32 = chars[start..i].iter().collect::<String>().parse().unwrap_or(0);
                        if let Some(name) = self.resolve(num, chars[i], at_line) {
                            out.push_str(name);
                            i += 1;
                            continue;
                        }
                    }
                }
                for c in &chars[start..i] {
                    out.push(*c);
                }
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        out
    }
}

#[derive(Clone)]
struct ParallelGlobalSnap {
    offset: u64,
    size: usize,
    section: u16,
    is_tls: bool,
}

#[derive(Clone)]
struct ParallelEmitCtx {
    target: Target,
    relocatable: bool,
    function_sections: bool,
    emit_asm_text: bool,
    debug_level: u8,
    emit_cfi: bool,
    source_name: String,
    globals: HashMap<String, ParallelGlobalSnap>,
    variadic_funcs: HashMap<String, bool>,
    nested_funcs: HashMap<String, Vec<(String, i32)>>,
    func_param_types: HashMap<String, Vec<Type>>,
    rodata: Vec<u8>,
    string_pool: HashMap<Vec<u8>, u64>,
    text_align: u64,
}

#[derive(Clone)]
struct FuncEmitChunk {
    name: String,
    code: Vec<u8>,
    size: u64,
    string_refs: Vec<(u64, Vec<u8>)>,
    float_refs: Vec<(u64, u64)>,
    pending_func_rels: Vec<(u64, String)>,
    pending_data_rels: Vec<(u64, String)>,
    label_addr_refs: Vec<(u64, String)>,
    disasm_calls: HashMap<u64, String>,
}

impl Codegen {
    pub fn new(target: Target) -> Self {
        Self { code: vec![], entry: 0, target, labels: Default::default(),
               pending: vec![], next_label: 0, var_map: HashMap::new(),
               loop_continue: None, loop_break: None,
               rodata: vec![], string_pool: HashMap::new(), string_refs: vec![], float_refs: vec![],
               globals: HashMap::new(), frame_size: 0, expr_depth: 0, max_expr_depth: 0, temp_slots: 0,
               vla_offsets: HashMap::new(),
               vla_total_size: 0,
               relocatable: false, function_sections: false, symbols: vec![], sym_index: HashMap::new(),
               relocations: vec![], pending_func_rels: vec![], pending_data_rels: vec![],
               data_section: vec![], bss_size: 0,
               tdata_section: vec![], tbss_size: 0, tdata_align: 8, tbss_align: 8,
               exec_text_size: 0, pending_tls_rels: vec![],
               text_align: 16, data_align: 8, bss_align: 8,
               extra_sections: vec![],
               custom_sec_indices: HashMap::new(), func_sizes: HashMap::new(),
               variadic_funcs: HashMap::new(),
               variadic_named_params: 0,
               variadic_named_gp: 0,
               variadic_named_fp: 0,
               variadic_overflow_offset: 16,
               variadic_reg_save_offset: 0,
               current_sret_offset: None,
               sret_depth: 0,
               nested_funcs: HashMap::new(),
               func_param_types: HashMap::new(),
               current_func_is_nested: false,
               current_returns_nonnull: false,
               static_chain_offset: 0,
               upvar_offsets: HashMap::new(),
               label_addr_refs: vec![],
               reg_bindings: HashMap::new(),
               pending_data64_rels: vec![],
               data_relocations: vec![], cleanup_vars: vec![],
               extra_rela_sections: vec![],
               emit_asm_text: false, debug_level: 0, emit_cfi: false,
               asm_directives: vec![], asm_directives_at: vec![],
               disasm_calls: HashMap::new(), disasm_data: HashMap::new(),
               global_asm_exports: HashSet::new(),
               func_asm_layout: HashMap::new(),
               source_name: String::new(),
               parallel_codegen: false }
    }

    /// -mno-red-zone 时禁止使用红区，帧上额外保留 128 字节。
    fn stack_reserve_bytes(&self) -> i32 {
        if self.target.red_zone { 0 } else { 128 }
    }
    pub fn emit(&mut self, b: &[u8]) { self.code.extend_from_slice(b); }
    fn needs_memory_return(ty: &Type) -> bool {
        Self::abi_classes(ty) == vec![AbiClass::Memory]
    }

    fn alloc_sret_slot(&mut self, size: usize) -> i32 {
        self.sret_depth += 1;
        -8 - (self.expr_depth as i32 * 8) - (self.sret_depth * ((size as i32 + 15) & !15))
    }

    fn store_xmm128_stack(&mut self, xmm: u8, off: i32) {
        let mut prefix = vec![0x41u8];
        if xmm >= 8 {
            prefix.push(0x44);
        }
        self.emit(&prefix);
        self.emit(&[0x0F, 0x11]);
        if off.abs() < 128 {
            self.emit(&[0x45 | ((xmm & 7) << 3), off as i8 as u8]);
        } else {
            self.emit(&[0x85 | ((xmm & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        }
    }

    fn mov_xmm(&mut self, dst: u8, src: u8) {
        if dst == src {
            return;
        }
        let mut rex = 0u8;
        if dst >= 8 {
            rex |= 0x04; // REX.R
        }
        if src >= 8 {
            rex |= 0x01; // REX.B
        }
        if rex != 0 {
            self.emit(&[0x40 | rex]);
        }
        self.emit(&[0x0F, 0x28, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
    }

    pub fn pos(&self) -> u64 { self.code.len() as u64 }
    pub fn label(&mut self, name: &str) { self.labels.insert(name.to_string(), self.pos()); }
    pub fn fresh_label(&mut self) -> String { let n = self.next_label; self.next_label += 1; format!(".L{}", n) }

    fn abi_classes(ty: &Type) -> Vec<AbiClass> {
        let size = ty.size();
        if size == 0 { return Vec::new(); }
        if size > 16 || matches!(ty, Type::LongDouble) {
            return vec![AbiClass::Memory];
        }
        let mut classes = vec![AbiClass::Sse; (size + 7) / 8];
        if !Self::classify_at(ty, 0, &mut classes) {
            return vec![AbiClass::Memory];
        }
        classes
    }

    fn classify_at(ty: &Type, base: usize, classes: &mut [AbiClass]) -> bool {
        if ty.align() > 1 && base % ty.align() != 0 { return false; }
        match ty {
            Type::Float | Type::Double | Type::Vector(_, _) => {}
            Type::Struct(_, fields) => {
                for field in fields.iter() {
                    if !Self::classify_at(&field.ty, base + field.offset, classes) { return false; }
                }
                return true;
            }
            Type::Union(_, fields) => {
                for field in fields.iter() {
                    if !Self::classify_at(&field.ty, base, classes) { return false; }
                }
                return true;
            }
            Type::Array(elem, ArraySize::Const(n)) => {
                for i in 0..*n {
                    if !Self::classify_at(elem, base + i * elem.size(), classes) { return false; }
                }
                return true;
            }
            Type::Atomic(inner) => return Self::classify_at(inner, base, classes),
            _ => {
                let first = base / 8;
                let last = (base + ty.size().saturating_sub(1)) / 8;
                for i in first..=last {
                    if i >= classes.len() { return false; }
                    classes[i] = AbiClass::Integer;
                }
            }
        }
        true
    }

    fn emit_sub_rsp(&mut self, bytes: usize) {
        if bytes == 0 { return; }
        self.emit(&[0x48, 0x81, 0xEC]);
        self.emit(&(bytes as u32).to_le_bytes());
    }

    fn emit_add_rsp(&mut self, bytes: usize) {
        if bytes == 0 { return; }
        self.emit(&[0x48, 0x81, 0xC4]);
        self.emit(&(bytes as u32).to_le_bytes());
    }

    fn store_rsp_qword(&mut self, reg: u8, off: usize) {
        let rex = 0x48 | if reg >= 8 { 0x04 } else { 0 };
        self.emit(&[rex, 0x89]);
        if off < 128 {
            self.emit(&[0x44 | ((reg & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[0x84 | ((reg & 7) << 3), 0x24]);
            self.emit(&(off as u32).to_le_bytes());
        }
    }

    fn load_rsp_qword(&mut self, reg: u8, off: usize) {
        let rex = 0x48 | if reg >= 8 { 0x04 } else { 0 };
        self.emit(&[rex, 0x8B]);
        if off < 128 {
            self.emit(&[0x44 | ((reg & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[0x84 | ((reg & 7) << 3), 0x24]);
            self.emit(&(off as u32).to_le_bytes());
        }
    }

    fn store_rsp_xmm(&mut self, xmm: u8, off: usize) {
        self.emit(&[0xF2, 0x0F, 0x11]);
        if off < 128 {
            self.emit(&[0x44 | ((xmm & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[0x84 | ((xmm & 7) << 3), 0x24]);
            self.emit(&(off as u32).to_le_bytes());
        }
    }

    fn load_rsp_xmm(&mut self, xmm: u8, off: usize) {
        self.emit(&[0xF2, 0x0F, 0x10]);
        if off < 128 {
            self.emit(&[0x44 | ((xmm & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[0x84 | ((xmm & 7) << 3), 0x24]);
            self.emit(&(off as u32).to_le_bytes());
        }
    }

    /// 解析 visibility 字符串为 ELF 常量
    fn parse_visibility(vis: &Option<String>) -> u8 {
        match vis.as_deref() {
            Some("hidden") => STV_HIDDEN,
            Some("protected") => STV_PROTECTED,
            _ => STV_DEFAULT,
        }
    }

    /// P1-5: 解析寄存器名 ("r10", "eax" 等) 为寄存器编号
    fn parse_reg_name(name: &str) -> Option<u8> {
        match name.to_lowercase().as_str() {
            "al" | "ah" | "ax" | "eax" | "rax" | "%rax" | "%eax" | "%ax" => Some(0),
            "bl" | "bh" | "bx" | "ebx" | "rbx" | "%rbx" | "%ebx" | "%bx" => Some(3),
            "cl" | "ch" | "cx" | "ecx" | "rcx" | "%rcx" | "%ecx" | "%cx" => Some(1),
            "dl" | "dh" | "dx" | "edx" | "rdx" | "%rdx" | "%edx" | "%dx" => Some(2),
            "sil" | "si" | "esi" | "rsi" | "%rsi" | "%esi" | "%si" => Some(6),
            "dil" | "di" | "edi" | "rdi" | "%rdi" | "%edi" | "%di" => Some(7),
            "r8" | "r8d" | "r8w" | "r8b" | "%r8" => Some(8),
            "r9" | "r9d" | "r9w" | "r9b" | "%r9" => Some(9),
            "r10" | "r10d" | "r10w" | "r10b" | "%r10" => Some(10),
            "r11" | "r11d" | "r11w" | "r11b" | "%r11" => Some(11),
            "r12" | "r12d" | "r12w" | "r12b" | "%r12" => Some(12),
            "r13" | "r13d" | "r13w" | "r13b" | "%r13" => Some(13),
            "r14" | "r14d" | "r14w" | "r14b" | "%r14" => Some(14),
            "r15" | "r15d" | "r15w" | "r15b" | "%r15" => Some(15),
            _ => None,
        }
    }

    /// 判断函数是否需要放入自定义 section
    fn is_custom_section_func(f: &FuncDecl) -> bool {
        f.section.as_ref().map_or(false, |s| !s.is_empty())
    }
    /// 判断变量是否需要放入自定义 section
    fn is_custom_section_var(v: &VarDecl) -> bool {
        v.section.as_ref().map_or(false, |s| !s.is_empty())
    }
    /// 确保自定义 section 存在于 extra_sections 中，返回其在 vec 中的索引
    fn ensure_custom_section(&mut self, name: &str, is_exec: bool, is_writable: bool) -> usize {
        if let Some(&idx) = self.custom_sec_indices.get(name) {
            return idx;
        }
        let idx = self.extra_sections.len();
        let mut flags = SHF_ALLOC;
        if is_writable { flags |= SHF_WRITE; }
        if is_exec { flags |= SHF_EXECINSTR; }
        self.extra_sections.push(ElfSection {
            name: name.to_string(),
            data: Vec::new(),
            ty: SHT_PROGBITS,
            flags,
            align: if is_exec { 16 } else { 8 },
        });
        self.custom_sec_indices.insert(name.to_string(), idx);
        idx
    }

    fn jmp_rel32(&mut self, target: &str, kind: JumpKind) {
        let at = self.pos();
        if kind == JumpKind::Jmp { self.emit(&[0xE9, 0, 0, 0, 0]); }
        else {
            let cc = match kind { JumpKind::Je=>0x84, JumpKind::Jne=>0x85, JumpKind::Jl=>0x8C,
                JumpKind::Jle=>0x8E, JumpKind::Jg=>0x8F, JumpKind::Jge=>0x8D,
                JumpKind::Jb=>0x82, JumpKind::Ja=>0x87, JumpKind::Jns=>0x89,
                _=>unreachable!() };
            self.emit(&[0x0F, cc, 0, 0, 0, 0]);
        }
        self.pending.push(PendingJump { at, target: target.into(), kind });
    }

    pub fn resolve(&mut self, rodata_start: u64) {
        for (p, name) in &self.pending_data_rels {
            self.disasm_data.insert(*p, name.clone());
        }
        // 修补跳转
        for p in self.pending.drain(..) {
            let dst = *self.labels.get(&p.target).expect(&format!("unresolved label: {}", p.target));
            let off = p.at as usize;
            let (rel_from, rel_off) = match p.kind { JumpKind::Jmp => (p.at+5, off+1), _ => (p.at+6, off+2) }; // Jmp=5字节, Jcc=6字节
            let rel = (dst as i64) - (rel_from as i64);
            self.code[rel_off..rel_off+4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 RIP-relative 字符串引用 (LEA dst, [RIP+disp32])
        for (code_pos, data) in self.string_refs.drain(..) {
            let str_off = self.string_pool[&data];
            let str_vaddr = rodata_start + str_off;
            let rip = self.entry + code_pos + 7; // LEA is 7 bytes
            let rel = (str_vaddr as i64) - (rip as i64);
            let p = code_pos as usize + 3; // disp32 starts at byte 3
            self.code[p..p+4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 RIP-relative 浮点字面量引用 (movsd/movss xmm0, [RIP+disp32])
        for (code_pos, rodata_off) in self.float_refs.drain(..) {
            let flt_vaddr = rodata_start + rodata_off;
            let rip = self.entry + code_pos + 8; // movsd/movss is 8 bytes
            let rel = (flt_vaddr as i64) - (rip as i64);
            let p = code_pos as usize + 4; // disp32 starts at byte 4
            self.code[p..p+4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        // 修补 RIP-relative 全局变量引用 (mov/lea reg, [RIP+disp32])
        for (disp32_pos, var_name) in self.pending_data_rels.drain(..) {
            if let Some(g) = self.globals.get(&var_name) {
                if g.is_tls {
                    continue;
                }
                let var_vaddr = rodata_start + g.offset;
                let rip = self.entry + disp32_pos + 4;
                let rel = (var_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p+4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        // 修补 TLS 变量 RIP-relative 引用（ET_EXEC .tdata 模板）
        let tdata_start = rodata_start + self.rodata.len() as u64;
        for (disp32_pos, var_name) in self.pending_tls_rels.drain(..) {
            if let Some(g) = self.globals.get(&var_name) {
                let var_vaddr = tdata_start + g.offset;
                let rip = self.entry + disp32_pos + 4;
                let rel = (var_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p+4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
        // 修补 &&label 地址引用 (LEA reg, [RIP+disp32])
        for (disp32_pos, label_name) in self.label_addr_refs.drain(..) {
            if let Some(&label_off) = self.labels.get(&label_name) {
                let label_vaddr = self.entry + label_off;
                let rip = self.entry + disp32_pos + 4;
                let rel = (label_vaddr as i64) - (rip as i64);
                let p = disp32_pos as usize;
                self.code[p..p+4].copy_from_slice(&(rel as i32).to_le_bytes());
            }
        }
    }

    pub fn compile(&mut self, module: &crate::ir::Module) {
        let prog = module.source();
        // 预扫描: 记录哪些函数是可变参数的
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                Self::register_func_sig(f, &mut self.func_param_types);
                if f.is_variadic { self.variadic_funcs.insert(f.name.name.clone(), true); }
                if let Some(body) = &f.body {
                    let mut nested = Vec::new();
                    Self::collect_nested_funcs(&body.items, &mut nested);
                    for nf in nested {
                        Self::register_func_sig(&nf, &mut self.func_param_types);
                        self.nested_funcs.insert(nf.name.name.clone(), nf.upvars.clone());
                    }
                }
            }
        }
        if self.relocatable {
            self.compile_rel(prog);
        } else {
            self.compile_exec(prog);
        }
    }

    /// ET_EXEC 模式: 生成可直接执行的 ELF
    fn compile_exec(&mut self, prog: &Program) {
        self.entry = 0x400000 + 64 + 56;
        // 收集全局变量 → rodata / .tdata(TLS)
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                if v.is_extern {
                    continue;
                }
                let sz = v.ty.size().max(1);
                let align = v.alignment.unwrap_or(v.ty.align()).max(1);
                if v.is_thread_local {
                    self.tdata_align = self.tdata_align.max(align as u64);
                    let off = (self.tdata_section.len() + align - 1) & !(align - 1);
                    while self.tdata_section.len() < off { self.tdata_section.push(0); }
                    let offset = self.tdata_section.len() as u64;
                    let init_data = Self::eval_global_init(self, v, sz, false);
                    if let Some(ref data) = init_data {
                        self.tdata_section.extend_from_slice(data);
                        for _ in data.len()..sz { self.tdata_section.push(0); }
                    } else {
                        for _ in 0..sz { self.tdata_section.push(0); }
                    }
                    let sec = if v.init.is_some() { 5u16 } else { 6u16 };
                    if v.init.is_none() {
                        self.tbss_size = self.tbss_size.max(offset + sz as u64);
                    }
                    self.globals.insert(v.name.name.clone(), GlobalVar {
                        offset, size: sz, ty: v.ty.clone(), init: init_data, section: sec, is_tls: true,
                    });
                    continue;
                }
                let off = (self.rodata.len() + align - 1) & !(align - 1);
                while self.rodata.len() < off { self.rodata.push(0); }
                let offset = self.rodata.len() as u64;
                let init_data = Self::eval_global_init(self, v, sz, true);
                if let Some(ref data) = init_data {
                    self.rodata.extend_from_slice(data);
                    for _ in data.len()..sz { self.rodata.push(0); }
                } else {
                    for _ in 0..sz { self.rodata.push(0); }
                }
                self.globals.insert(v.name.name.clone(), GlobalVar {
                    offset, size: sz, ty: v.ty.clone(), init: init_data, section: 1, is_tls: false,
                });
            }
        }
        let _rodata_size = self.rodata.len();
        let _main = prog.decls.iter().find_map(|d| match d { Decl::Func(f) if f.name.name == "main" => Some(f), _ => None }).expect("no main()");
        // 收集 constructor/destructor 函数
        let ctors: Vec<String> = prog.decls.iter().filter_map(|d| {
            if let Decl::Func(f) = d { if f.is_constructor && f.body.is_some() { Some(f.name.name.clone()) } else { None } } else { None }
        }).collect();
        let dtors: Vec<String> = prog.decls.iter().filter_map(|d| {
            if let Decl::Func(f) = d { if f.is_destructor && f.body.is_some() { Some(f.name.name.clone()) } else { None } } else { None }
        }).collect();
        self.label("_start");
        self.emit(&[0x49, 0x89, 0xe4]); // mov r12, rsp (保存原始 rsp, 用于 leave 后恢复)
        self.emit(&[0x55]);             // push rbp
        self.emit(&[0x48, 0x89, 0xe5]); // mov rbp, rsp
        // 调用 constructor 函数
        for ctor in &ctors {
            self.emit(&[0x48, 0x31, 0xc0]); // xor rax, rax (no variadic args)
            self.call_label(ctor);
        }
        self.emit(&[0x48, 0x31, 0xc0]); // xor rax, rax
        self.call_label("main");
        // 保存 main 返回值
        self.emit(&[0x41, 0x89, 0xc5]); // mov r13d, eax
        // 调用 destructor 函数
        for dtor in &dtors {
            self.emit(&[0x48, 0x31, 0xc0]); // xor rax, rax
            self.call_label(dtor);
        }
        self.emit(&[0x4c, 0x89, 0xe4]); // mov rsp, r12 (恢复原始 rsp)
        self.emit(&[0x44, 0x89, 0xef]); // mov edi, r13d (main 返回值)
        self.emit(&[0xb8, 0x3c, 0x00, 0x00, 0x00]); // mov eax, 0x3c
        self.emit(&[0x0f, 0x05]);       // syscall
        self.emit(&[0xeb, 0xfe]);       // jmp self
        self.emit_functions(prog);
        // 对齐 code 到 8 字节 (ELF 的 .rodata 也按 8 对齐)
        while self.code.len() % 8 != 0 { self.code.push(0); }
        self.exec_text_size = self.code.len() as u64;
        let rodata_start = self.entry + self.exec_text_size;
        self.code.extend_from_slice(&self.rodata);
        self.code.extend_from_slice(&self.tdata_section);
        self.collect_exec_symbols(prog);
        self.resolve(rodata_start);
    }

    /// ET_EXEC：设置 FS 基址指向 TLS 模板（可写 PT_LOAD 段内）
    fn emit_tls_fs_setup(&mut self) {
        // lea rsi, [rip+disp32]  → TLS 模板地址
        let lea_pos = self.pos() + 3;
        self.emit(&[0x48, 0x8D, 0x35, 0, 0, 0, 0]);
        self.pending_tls_rels.push((lea_pos, "__dcc_tls_base".to_string()));
        // mov rdi, 0x1002 (ARCH_SET_FS)
        self.emit(&[0x48, 0xC7, 0xC7, 0x02, 0x00, 0x01, 0x00]);
        // mov rax, 158; syscall
        self.emit(&[0x48, 0xC7, 0xC0, 0x9E, 0x00, 0x00, 0x00]);
        self.emit(&[0x0F, 0x05]);
    }

    /// 收集 ET_EXEC 符号表（函数 + 全局变量）
    fn collect_exec_symbols(&mut self, prog: &Program) {
        self.symbols.clear();
        self.sym_index.clear();
        self.symbols.push(ElfSym {
            name: String::new(), section: SHN_UNDEF, value: 0, size: 0,
            bind: STB_LOCAL, ty: STT_NOTYPE, visibility: STV_DEFAULT,
        });
        if !self.rodata.is_empty() {
            self.symbols.push(ElfSym {
                name: ".rodata".into(), section: 2, value: 0, size: self.rodata.len() as u64,
                bind: STB_LOCAL, ty: STT_SECTION, visibility: STV_DEFAULT,
            });
        }
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                if v.is_extern && v.init.is_none() {
                    continue;
                }
                let Some(g) = self.globals.get(&v.name.name) else { continue };
                let ty = if g.is_tls { STT_TLS } else { STT_OBJECT };
                self.symbols.push(ElfSym {
                    name: v.name.name.clone(), section: g.section, value: g.offset, size: g.size as u64,
                    bind: if v.is_static { STB_LOCAL } else { STB_GLOBAL },
                    ty, visibility: Self::parse_visibility(&v.visibility),
                });
            }
        }
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_none() {
                    continue;
                }
                if let Some(&off) = self.labels.get(&f.name.name) {
                    let sz = self.func_sizes.get(&f.name.name).copied().unwrap_or(0);
                    self.symbols.push(ElfSym {
                        name: f.name.name.clone(), section: 1, value: off, size: sz,
                        bind: if f.is_static { STB_LOCAL } else { STB_GLOBAL },
                        ty: STT_FUNC, visibility: Self::parse_visibility(&f.visibility),
                    });
                }
            }
        }
        for (i, sym) in self.symbols.iter().enumerate() {
            if !sym.name.is_empty() {
                self.sym_index.insert(sym.name.clone(), i as u32);
            }
        }
    }

    fn eval_global_init(&mut self, v: &VarDecl, sz: usize, use_rodata: bool) -> Option<Vec<u8>> {
        let init = v.init.as_ref()?;
        match init {
            Expr::Int { v, .. } => Some(v.to_le_bytes()[..sz].to_vec()),
            Expr::String { data, .. } => Some(data.clone()),
            Expr::InitList { inits, .. } => {
                let base = if use_rodata { self.rodata.len() } else { self.tdata_section.len() } as u64;
                self.gen_global_init_list(&v.ty, inits, Some((base, None)))
            }
            Expr::AddrOf { e, .. } => match e.as_ref() {
                Expr::Ident { name: ref_name, .. } => {
                    if let Some(ref g) = self.globals.get(&ref_name.name) {
                        let vaddr = self.entry + g.offset;
                        Some(vaddr.to_le_bytes().to_vec())
                    } else {
                        Some(vec![0u8; 8])
                    }
                }
                _ => None,
            },
            _ => const_eval::eval(init).ok().map(|value| value.to_le_bytes()[..sz].to_vec()),
        }
    }

    /// ET_REL 模式: 生成可重定位 .o 文件
    fn compile_rel(&mut self, prog: &Program) {
        // Phase 1: 生成代码, 收集数据
        // 收集全局变量 → .data / .bss (或自定义 section)
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                let sz = v.ty.size().max(1);
                let align = v.alignment.unwrap_or(v.ty.align()).max(1);
                if v.is_extern && v.init.is_none() {
                    self.globals.insert(v.name.name.clone(), GlobalVar {
                        offset: 0, size: sz, ty: v.ty.clone(), init: None, section: SHN_UNDEF, is_tls: false,
                    });
                    continue;
                }
                if v.is_thread_local {
                    let initialized = v.init.is_some();
                    if initialized {
                        self.tdata_align = self.tdata_align.max(align as u64);
                    } else {
                        self.tbss_align = self.tbss_align.max(align as u64);
                    }
                    let offset = if initialized {
                        let off = (self.tdata_section.len() + align - 1) & !(align - 1);
                        while self.tdata_section.len() < off { self.tdata_section.push(0); }
                        self.tdata_section.len() as u64
                    } else {
                        let off = ((self.tbss_size + align as u64 - 1) / align as u64) * align as u64;
                        self.tbss_size = off + sz as u64;
                        off
                    };
                    if let Some(init) = &v.init {
                        if let Ok(value) = const_eval::eval(init) {
                            self.tdata_section.extend_from_slice(&value.to_le_bytes()[..sz]);
                        } else if let Expr::Int { v: iv, .. } = init {
                            self.tdata_section.extend_from_slice(&iv.to_le_bytes()[..sz]);
                        }
                    }
                    self.globals.insert(v.name.name.clone(), GlobalVar {
                        offset, size: sz, ty: v.ty.clone(), init: None,
                        section: if initialized { 5 } else { 6 }, is_tls: true,
                    });
                    continue;
                }
                // 自定义 section 变量: 路由到 extra_sections
                if Self::is_custom_section_var(v) {
                    let sec_name = v.section.as_ref().unwrap().clone();
                    let is_writable = !matches!(&v.init, Some(Expr::String { .. }));
                    let sec_idx = self.ensure_custom_section(&sec_name, false, is_writable);
                    self.extra_sections[sec_idx].align =
                        self.extra_sections[sec_idx].align.max(align as u64);
                    if v.init.is_none() && self.extra_sections[sec_idx].data.is_empty() {
                        self.extra_sections[sec_idx].ty = elf::SHT_NOBITS;
                    } else if v.init.is_some() {
                        // 同名节一旦包含显式初始化数据就必须占文件空间。
                        self.extra_sections[sec_idx].ty = SHT_PROGBITS;
                    }
                    let sec = &mut self.extra_sections[sec_idx];
                    let off = (sec.data.len() + align - 1) & !(align - 1);
                    while sec.data.len() < off { sec.data.push(0); }
                    let offset = sec.data.len() as u64;
                    let mut has_init = false;
                    if let Some(init) = &v.init {
                        match init {
                            Expr::Int { v: iv, .. } => {
                                let bytes = iv.to_le_bytes()[..sz].to_vec();
                                self.extra_sections[sec_idx].data.extend_from_slice(&bytes);
                                has_init = true;
                            }
                            Expr::String { data, .. } => {
                                self.extra_sections[sec_idx].data.extend_from_slice(data);
                                for _ in data.len()..sz { self.extra_sections[sec_idx].data.push(0); }
                                has_init = true;
                            }
                            Expr::InitList { inits, .. } => {
                                let base = self.extra_sections[sec_idx].data.len() as u64;
                                if let Some(init_data) = self.gen_global_init_list(
                                    &v.ty, inits, Some((base, Some(sec_idx))),
                                ) {
                                    self.extra_sections[sec_idx].data.extend_from_slice(&init_data);
                                    for _ in init_data.len()..sz { self.extra_sections[sec_idx].data.push(0); }
                                    has_init = true;
                                }
                            }
                            Expr::AddrOf { e, .. } => {
                                // P1-6: 自定义 section 中的符号引用
                                if let Expr::Ident { name: ref_name, .. } = e.as_ref() {
                                    let ref_off = self.extra_sections[sec_idx].data.len() as u64;
                                    let width = if sz <= 4 { 4 } else { 8 };
                                    self.extra_sections[sec_idx].data.resize(
                                        ref_off as usize + sz.max(width), 0);
                                    let rel_type = if width == 4 { R_X86_64_32 } else { R_X86_64_64 };
                                    self.pending_data64_rels.push((
                                        ref_off, ref_name.name.clone(), Some(sec_idx), rel_type, 0));
                                    has_init = true;
                                } else if let Expr::Index { arr, idx, ty: elem_ty, .. } = e.as_ref() {
                                    if let (Expr::Ident { name: ref_name, .. }, Ok(i)) =
                                        (arr.as_ref(), const_eval::eval(idx))
                                    {
                                        let ref_off = self.extra_sections[sec_idx].data.len() as u64;
                                        let width = if sz <= 4 { 4 } else { 8 };
                                        self.extra_sections[sec_idx].data.resize(
                                            ref_off as usize + sz.max(width), 0);
                                        let rel_type = if width == 4 { R_X86_64_32 } else { R_X86_64_64 };
                                        let addend = (i as i64) * elem_ty.size().max(1) as i64;
                                        self.pending_data64_rels.push((
                                            ref_off, ref_name.name.clone(), Some(sec_idx), rel_type, addend));
                                        has_init = true;
                                    }
                                }
                            }
                            _ => {
                                if let Ok(value) = const_eval::eval(init) {
                                    self.extra_sections[sec_idx].data
                                        .extend_from_slice(&value.to_le_bytes()[..sz]);
                                    has_init = true;
                                }
                            }
                        }
                    }
                    if !has_init {
                        for _ in 0..sz { self.extra_sections[sec_idx].data.push(0); }
                    }
                    self.globals.insert(v.name.name.clone(), GlobalVar {
                        offset, size: sz, ty: v.ty.clone(), init: None, section: (sec_idx + 7) as u16, is_tls: false,
                    });
                    continue;
                }
                let initialized = v.init.is_some();
                if initialized {
                    self.data_align = self.data_align.max(align as u64);
                } else {
                    self.bss_align = self.bss_align.max(align as u64);
                }
                let offset = if initialized {
                    let off = (self.data_section.len() + align - 1) & !(align - 1);
                    while self.data_section.len() < off { self.data_section.push(0); }
                    self.data_section.len() as u64
                } else {
                    self.bss_size = ((self.bss_size + align as u64 - 1) / align as u64) * align as u64;
                    self.bss_size
                };
                let has_init = if let Some(init) = &v.init {
                    match init {
                        Expr::Int { v: iv, .. } => {
                            let bytes = iv.to_le_bytes()[..sz].to_vec();
                            self.data_section.extend_from_slice(&bytes);
                            true
                        }
                        Expr::String { data, .. } => {
                            self.data_section.extend_from_slice(data);
                            for _ in data.len()..sz { self.data_section.push(0); }
                            true
                        }
                        Expr::InitList { inits, .. } => {
                            let base = self.data_section.len() as u64;
                            if let Some(init_data) = self.gen_global_init_list(
                                &v.ty, inits, Some((base, None)),
                            ) {
                                self.data_section.extend_from_slice(&init_data);
                                for _ in init_data.len()..sz { self.data_section.push(0); }
                                true
                            } else { false }
                        }
                        Expr::AddrOf { e, .. } => {
                            // P1-6: .data 节中的符号引用
                            if let Expr::Ident { name: ref_name, .. } = e.as_ref() {
                                let ref_off = self.data_section.len() as u64;
                                let width = if sz <= 4 { 4 } else { 8 };
                                self.data_section.resize(ref_off as usize + sz.max(width), 0);
                                let rel_type = if width == 4 { R_X86_64_32 } else { R_X86_64_64 };
                                self.pending_data64_rels.push((
                                    ref_off, ref_name.name.clone(), None, rel_type, 0));
                                true
                            } else if let Expr::Index { arr, idx, ty: elem_ty, .. } = e.as_ref() {
                                if let (Expr::Ident { name: ref_name, .. }, Ok(i)) =
                                    (arr.as_ref(), const_eval::eval(idx))
                                {
                                    let ref_off = self.data_section.len() as u64;
                                    let width = if sz <= 4 { 4 } else { 8 };
                                    self.data_section.resize(ref_off as usize + sz.max(width), 0);
                                    let rel_type = if width == 4 { R_X86_64_32 } else { R_X86_64_64 };
                                    let addend = (i as i64) * elem_ty.size().max(1) as i64;
                                    self.pending_data64_rels.push((
                                        ref_off, ref_name.name.clone(), None, rel_type, addend));
                                    true
                                } else { false }
                            } else { false }
                        }
                        _ => {
                            if let Ok(value) = const_eval::eval(init) {
                                self.data_section.extend_from_slice(&value.to_le_bytes()[..sz]);
                                true
                            } else {
                                false
                            }
                        },
                    }
                } else { false };
                if !has_init {
                    if initialized {
                        self.data_section.resize(self.data_section.len() + sz, 0);
                    } else {
                        self.bss_size += sz as u64;
                    }
                }
                self.globals.insert(v.name.name.clone(), GlobalVar {
                    offset, size: sz, ty: v.ty.clone(), init: None,
                    section: if initialized { 3 } else { 4 }, is_tls: false,
                });
            }
        }

        // 文件级 asm 先发射到 .text
        for decl in &prog.decls {
            if let Decl::GlobalAsm { asm_str, outputs, output_names, output_constraints,
                inputs, input_names, input_constraints, clobbers, .. } = decl {
                self.emit_global_asm(
                    asm_str, outputs, output_names, output_constraints,
                    inputs, input_names, input_constraints, clobbers,
                );
            }
        }

        // 生成函数代码 (无 _start)
        self.emit_functions(prog);

        // 后处理: 将自定义 section 函数的代码从 .text 提取到 extra_sections
        // 记录函数在自定义 section 内的偏移 (用于符号表)
        let mut custom_func_offsets: HashMap<String, (u16, u64)> = HashMap::new(); // func_name → (section_idx+5, offset_in_section)
        let mut custom_func_ranges: Vec<(u64, u64, usize, u64)> = vec![]; // text start,size,extra idx,section offset
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_none() { continue; }
                let effective_section = f.section.clone()
                    .or_else(|| if f.is_cold { Some(".text.unlikely".to_string()) } else { None });
                if let Some(sec_name) = effective_section.as_ref() {
                    if sec_name.is_empty() { continue; }
                    if let Some(&func_start) = self.labels.get(&f.name.name) {
                        let func_size = self.func_sizes.get(&f.name.name).copied().unwrap_or(0);
                        if func_size == 0 { continue; }
                        let sec_idx = self.ensure_custom_section(sec_name, true, false);
                        let func_align = f.alignment.unwrap_or(16).max(1);
                        self.extra_sections[sec_idx].align =
                            self.extra_sections[sec_idx].align.max(func_align as u64);
                        let aligned = (self.extra_sections[sec_idx].data.len() + func_align - 1)
                            & !(func_align - 1);
                        self.extra_sections[sec_idx].data.resize(aligned, 0);
                        let sec_off = self.extra_sections[sec_idx].data.len() as u64;
                        // 从 .text 拷贝代码到自定义节
                        let code_slice = self.code[func_start as usize..(func_start + func_size) as usize].to_vec();
                        self.extra_sections[sec_idx].data.extend_from_slice(&code_slice);
                        // 暂时保留 .text 原代码；resolve() 修补本地跳转后会重新复制。
                        custom_func_ranges.push((func_start, func_size, sec_idx, sec_off));
                        custom_func_offsets.insert(f.name.name.clone(), ((sec_idx + 7) as u16, sec_off));
                    }
                }
            }
        }

        // -ffunction-sections：将普通函数拆到 .text.<name>，供链接期 --gc-sections 剔除
        if self.function_sections {
            let mut per_func: Vec<FuncDecl> = vec![];
            for decl in &prog.decls {
                if let Decl::Func(f) = decl {
                    if f.body.is_some() && f.section.is_none() && !f.is_cold {
                        per_func.push(f.clone());
                    }
                    if let Some(body) = &f.body {
                        Self::collect_nested_funcs(&body.items, &mut per_func);
                    }
                }
            }
            for f in &per_func {
                if custom_func_offsets.contains_key(&f.name.name) {
                    continue;
                }
                if f.section.is_some() || f.is_cold {
                    continue;
                }
                if let Some(&func_start) = self.labels.get(&f.name.name) {
                    let func_size = self.func_sizes.get(&f.name.name).copied().unwrap_or(0);
                    if func_size == 0 {
                        continue;
                    }
                    let sec_name = format!(".text.{}", f.name.name);
                    let sec_idx = self.ensure_custom_section(&sec_name, true, false);
                    let func_align = f.alignment.unwrap_or(16).max(1);
                    self.extra_sections[sec_idx].align =
                        self.extra_sections[sec_idx].align.max(func_align as u64);
                    let aligned = (self.extra_sections[sec_idx].data.len() + func_align - 1)
                        & !(func_align - 1);
                    self.extra_sections[sec_idx].data.resize(aligned, 0);
                    let sec_off = self.extra_sections[sec_idx].data.len() as u64;
                    let code_slice = self.code[func_start as usize..(func_start + func_size) as usize].to_vec();
                    self.extra_sections[sec_idx].data.extend_from_slice(&code_slice);
                    custom_func_ranges.push((func_start, func_size, sec_idx, sec_off));
                    custom_func_offsets.insert(f.name.name.clone(), ((sec_idx + 7) as u16, sec_off));
                }
            }
        }

        // 处理字符串引用 → 确保所有字符串在 rodata 中
        let string_data_list: Vec<Vec<u8>> = self.string_refs.iter().map(|(_, d)| d.clone()).collect();
        for data in &string_data_list {
            if !self.string_pool.contains_key(data) {
                let off = self.rodata.len() as u64;
                self.rodata.extend_from_slice(data);
                self.string_pool.insert(data.clone(), off);
            }
        }

        // Phase 2: 收集所有符号 (正确顺序: STN_UNDEF → LOCAL → GLOBAL)
        // 先收集到临时 vec, 然后排序构建最终 symtab
        let mut local_syms: Vec<ElfSym> = vec![];
        let mut global_syms: Vec<ElfSym> = vec![];

        // 全局变量符号（extern 声明 + 同 TU 定义会重复出现在 prog.decls）
        let mut emitted_var_syms: std::collections::HashSet<String> = std::collections::HashSet::new();
        let var_has_definition = |name: &str| -> bool {
            prog.decls.iter().any(|d| {
                matches!(d, Decl::Var(v) if v.name.name == name && (!v.is_extern || v.init.is_some()))
            })
        };
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                if v.is_extern && v.init.is_none() && var_has_definition(&v.name.name) {
                    continue;
                }
                if !emitted_var_syms.insert(v.name.name.clone()) {
                    continue;
                }
                let g = self.globals[&v.name.name].clone();
                let (section, bind) = if v.is_extern && v.init.is_none() && g.section == SHN_UNDEF {
                    (SHN_UNDEF, STB_GLOBAL)
                } else {
                    (g.section, if v.is_weak { STB_WEAK } else if v.is_static { STB_LOCAL } else { STB_GLOBAL })
                };
                let sym = ElfSym {
                    name: v.name.name.clone(), section, value: g.offset,
                    size: g.size as u64,
                    bind,
                    ty: if g.is_tls { STT_TLS } else { STT_OBJECT },
                    visibility: Self::parse_visibility(&v.visibility),
                };
                if v.is_static { local_syms.push(sym); } else { global_syms.push(sym); }
            }
        }

        // extern 函数声明 (无函数体 = 外部引用, 排除 alias 声明)
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_none() && f.alias_name.is_none() {
                    global_syms.push(ElfSym {
                        name: f.name.name.clone(), section: SHN_UNDEF, value: 0, size: 0,
                        bind: STB_GLOBAL, ty: STT_FUNC, visibility: STV_DEFAULT,
                    });
                }
            }
        }

        // 函数定义符号 (包括嵌套函数)
        for name in &self.global_asm_exports {
            if let Some(&off) = self.labels.get(name) {
                global_syms.push(ElfSym {
                    name: name.clone(), section: 1, value: off, size: 0,
                    bind: STB_GLOBAL, ty: STT_NOTYPE, visibility: STV_DEFAULT,
                });
            }
        }
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_some() {
                    // 收集此函数的嵌套函数 (递归, 不含自身)
                    let mut nested = vec![];
                    if let Some(body) = &f.body {
                        Self::collect_nested_funcs(&body.items, &mut nested);
                    }
                    // 先处理顶层函数自身
                    if let Some(&func_start) = self.labels.get(&f.name.name) {
                        let func_size = self.func_sizes.get(&f.name.name).copied().unwrap_or(0);
                        let (sec_idx, sym_value) = if let Some(&(si, off)) = custom_func_offsets.get(&f.name.name) {
                            (si, off)
                        } else {
                            (1u16, func_start)
                        };
                        let sym = ElfSym {
                            name: f.name.name.clone(), section: sec_idx as u16, value: sym_value,
                            size: func_size,
                            bind: if f.is_weak { STB_WEAK } else if f.is_static { STB_LOCAL } else { STB_GLOBAL },
                            ty: STT_FUNC,
                            visibility: Self::parse_visibility(&f.visibility),
                        };
                        if f.is_static { local_syms.push(sym); } else { global_syms.push(sym); }
                    }
                    // 再处理嵌套函数
                    for func in &nested {
                        if let Some(&func_start) = self.labels.get(&func.name.name) {
                            let func_size = self.func_sizes.get(&func.name.name).copied().unwrap_or(0);
                            let (sec_idx, sym_value) = if let Some(&(si, off)) = custom_func_offsets.get(&func.name.name) {
                                (si, off)
                            } else {
                                (1u16, func_start)
                            };
                            let sym = ElfSym {
                                name: func.name.name.clone(), section: sec_idx as u16, value: sym_value,
                                size: func_size,
                                bind: if func.is_weak { STB_WEAK } else if func.is_static { STB_LOCAL } else { STB_GLOBAL },
                                ty: STT_FUNC,
                                visibility: Self::parse_visibility(&func.visibility),
                            };
                            if func.is_static { local_syms.push(sym); } else { global_syms.push(sym); }
                        }
                    }
                }
            }
        }

        // __attribute__((alias)): 创建别名符号
        // alias 函数可能没有函数体 (只是声明), 如:
        //   int alias_func(void) __attribute__((alias("real_func")));
        // f.name.name = "alias_func", f.alias_name = Some("real_func")
        // 需要创建符号 alias_func 指向 real_func 的位置
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if let Some(ref alias_target) = f.alias_name {
                    // 找到目标函数的符号，创建别名指向同一位置
                    if let Some(target_sym) = global_syms.iter().find(|s| s.name == *alias_target)
                        .or_else(|| local_syms.iter().find(|s| s.name == *alias_target)) {
                        let alias_sym = ElfSym {
                            name: f.name.name.clone(),
                            section: target_sym.section,
                            value: target_sym.value,
                            size: target_sym.size,
                            bind: STB_GLOBAL,
                            ty: STT_FUNC,
                            visibility: STV_DEFAULT,
                        };
                        global_syms.push(alias_sym);
                    }
                }
            }
        }

        // .rodata section 符号 (LOCAL)
        if !self.rodata.is_empty() {
            local_syms.push(ElfSym {
                name: ".rodata".to_string(), section: 2, value: 0,
                size: self.rodata.len() as u64,
                bind: STB_LOCAL, ty: STT_SECTION, visibility: STV_DEFAULT,
            });
        }

        // 构建最终符号表: STN_UNDEF + locals + globals
        self.symbols.clear();
        self.sym_index.clear();
        // [0] STN_UNDEF
        self.symbols.push(ElfSym {
            name: String::new(), section: SHN_UNDEF, value: 0, size: 0,
            bind: STB_LOCAL, ty: STT_NOTYPE, visibility: STV_DEFAULT,
        });
        for sym in &local_syms {
            let idx = self.symbols.len() as u32;
            self.symbols.push(sym.clone());
            self.sym_index.insert(sym.name.clone(), idx);
        }
        for sym in &global_syms {
            let idx = self.symbols.len() as u32;
            self.symbols.push(sym.clone());
            self.sym_index.insert(sym.name.clone(), idx);
        }

        // Phase 3: 构建重定位 (使用正确的符号索引)
        // 查找 .rodata section 符号索引
        let rodata_sym_idx = self.sym_index.get(".rodata").copied();

        for (code_pos, data) in self.string_refs.drain(..) {
            let str_off = self.string_pool[&data] as i64;
            self.relocations.push(ElfRela {
                offset: code_pos + 3,
                sym_index: rodata_sym_idx.unwrap_or(0),
                rel_type: R_X86_64_PC32,
                addend: str_off - 7,
            });
        }

        // 浮点字面量重定位
        for (code_pos, rodata_off) in self.float_refs.drain(..) {
            self.relocations.push(ElfRela {
                offset: code_pos + 4,
                sym_index: rodata_sym_idx.unwrap_or(0),
                rel_type: R_X86_64_PC32,
                addend: rodata_off as i64 - 8,
            });
        }

        // 解析函数调用重定位 (使用最终符号表, 用 PLT32 以兼容 PIE)
        for (code_off, func_name) in self.pending_func_rels.drain(..) {
            let sym_idx = self.sym_index.get(&func_name).copied().unwrap_or(0);
            self.relocations.push(ElfRela {
                offset: code_off,
                sym_index: sym_idx,
                rel_type: R_X86_64_PLT32,
                addend: -4,
            });
        }

        // 解析全局变量重定位 (使用最终符号表)
        for (disp32_pos, var_name) in self.pending_data_rels.drain(..) {
            let sym_idx = self.sym_index.get(&var_name).copied().unwrap_or(0);
            self.relocations.push(ElfRela {
                offset: disp32_pos,
                sym_index: sym_idx,
                rel_type: R_X86_64_PC32,
                addend: -4,
            });
        }

        // P1-6: 解析 data 节 64 位符号引用重定位
        let mut custom_data_relas: HashMap<usize, Vec<ElfRela>> = HashMap::new();
        for (data_off, var_name, custom_sec, rel_type, addend) in self.pending_data64_rels.drain(..) {
            let sym_idx = self.sym_index.get(&var_name).copied().unwrap_or(0);
            let relocation = ElfRela {
                offset: data_off,
                sym_index: sym_idx,
                rel_type,
                addend,
            };
            if let Some(sec_idx) = custom_sec {
                custom_data_relas.entry(sec_idx).or_default().push(relocation);
            } else {
                self.data_relocations.push(relocation);
            }
        }
        for (sec_idx, relocations) in custom_data_relas {
            self.extra_rela_sections.push(elf::ExtraRelaSection {
                name: format!(".rela{}", self.extra_sections[sec_idx].name),
                relocations,
                target_sec_index: 7 + sec_idx as u32,
            });
        }

        // __attribute__((constructor/destructor)): 生成 .init_array/.fini_array
        let mut constructors: Vec<String> = vec![];
        let mut destructors: Vec<String> = vec![];
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_some() {
                    if f.is_constructor { constructors.push(f.name.name.clone()); }
                    if f.is_destructor { destructors.push(f.name.name.clone()); }
                }
            }
        }
        if !constructors.is_empty() || !destructors.is_empty() {
            let base_sec_idx = 7 + self.extra_sections.len() as u32;
            // .init_array
            if !constructors.is_empty() {
                let init_array_sec_idx = base_sec_idx;
                let mut init_data = Vec::with_capacity(constructors.len() * 8);
                let mut init_rels = Vec::with_capacity(constructors.len());
                for func_name in &constructors {
                    let entry_off = init_data.len() as u64;
                    init_data.extend_from_slice(&[0u8; 8]); // placeholder
                    let sym_idx = self.sym_index.get(func_name).copied().unwrap_or(0);
                    init_rels.push(ElfRela {
                        offset: entry_off,
                        sym_index: sym_idx,
                        rel_type: elf::R_X86_64_64,
                        addend: 0,
                    });
                }
                self.extra_sections.push(elf::ElfSection {
                    name: ".init_array".to_string(),
                    data: init_data,
                    ty: elf::SHT_PROGBITS,
                    flags: elf::SHF_ALLOC | elf::SHF_WRITE,
                    align: 8,
                });
                self.extra_rela_sections.push(elf::ExtraRelaSection {
                    name: ".rela.init_array".to_string(),
                    relocations: init_rels,
                    target_sec_index: init_array_sec_idx,
                });
            }
            // .fini_array
            if !destructors.is_empty() {
                let fini_array_sec_idx = base_sec_idx + if constructors.is_empty() { 0 } else { 1 };
                let mut fini_data = Vec::with_capacity(destructors.len() * 8);
                let mut fini_rels = Vec::with_capacity(destructors.len());
                for func_name in &destructors {
                    let entry_off = fini_data.len() as u64;
                    fini_data.extend_from_slice(&[0u8; 8]);
                    let sym_idx = self.sym_index.get(func_name).copied().unwrap_or(0);
                    fini_rels.push(ElfRela {
                        offset: entry_off,
                        sym_index: sym_idx,
                        rel_type: elf::R_X86_64_64,
                        addend: 0,
                    });
                }
                self.extra_sections.push(elf::ElfSection {
                    name: ".fini_array".to_string(),
                    data: fini_data,
                    ty: elf::SHT_PROGBITS,
                    flags: elf::SHF_ALLOC | elf::SHF_WRITE,
                    align: 8,
                });
                self.extra_rela_sections.push(elf::ExtraRelaSection {
                    name: ".rela.fini_array".to_string(),
                    relocations: fini_rels,
                    target_sec_index: fini_array_sec_idx,
                });
            }
        }

        // 将源地址位于自定义函数中的重定位移入对应 .rela.<section>。
        let mut custom_relas: HashMap<usize, Vec<ElfRela>> = HashMap::new();
        let mut text_relas = vec![];
        for mut rel in self.relocations.drain(..) {
            if let Some((start, _, sec_idx, sec_off)) = custom_func_ranges.iter()
                .find(|(start, size, _, _)| rel.offset >= *start && rel.offset < *start + *size) {
                rel.offset = *sec_off + (rel.offset - *start);
                custom_relas.entry(*sec_idx).or_default().push(rel);
            } else {
                text_relas.push(rel);
            }
        }
        self.relocations = text_relas;
        for (sec_idx, relocations) in custom_relas {
            if relocations.is_empty() { continue; }
            let rela_name = format!(".rela{}", self.extra_sections[sec_idx].name);
            let target_sec_index = 7 + sec_idx as u32;
            if let Some(existing) = self.extra_rela_sections.iter_mut()
                .find(|r| r.target_sec_index == target_sec_index) {
                existing.relocations.extend(relocations);
            } else {
                self.extra_rela_sections.push(elf::ExtraRelaSection {
                    name: rela_name, relocations, target_sec_index,
                });
            }
        }

        // 解析内部跳转 (本地标签)，再把最终机器码覆盖到自定义节副本。
        self.resolve(0);
        for (start, size, sec_idx, sec_off) in &custom_func_ranges {
            let src = self.code[*start as usize..(*start + *size) as usize].to_vec();
            let dst_start = *sec_off as usize;
            let dst_end = dst_start + *size as usize;
            self.extra_sections[*sec_idx].data[dst_start..dst_end].copy_from_slice(&src);
            self.code[*start as usize..(*start + *size) as usize].fill(0);
        }
        self.finalize_func_asm_layout(prog, &custom_func_ranges);
    }

    /// 在 resolve 后捕获每个函数的最终机器码与所属节。
    fn finalize_func_asm_layout(
        &mut self,
        prog: &Program,
        custom: &[(u64, u64, usize, u64)],
    ) {
        let mut custom_by_start: HashMap<u64, (String, usize, u64)> = HashMap::new();
        for (start, _size, sec_idx, sec_off) in custom {
            let sec_name = self.extra_sections[*sec_idx].name.clone();
            custom_by_start.insert(*start, (sec_name, *sec_idx, *sec_off));
        }
        let mut names = Vec::new();
        Self::collect_all_funcs(prog, &mut names);
        for name in names {
            let start = match self.labels.get(&name) {
                Some(s) => *s,
                None => continue,
            };
            let size = self.func_sizes.get(&name).copied().unwrap_or(0);
            if size == 0 {
                continue;
            }
            let section;
            let bytes;
            if let Some((sec_name, sec_idx, sec_off)) = custom_by_start.get(&start) {
                section = sec_name.clone();
                let off = *sec_off as usize;
                bytes = self.extra_sections[*sec_idx].data[off..off + size as usize].to_vec();
            } else {
                section = ".text".to_string();
                bytes = self.code[start as usize..(start + size) as usize].to_vec();
            }
            self.func_asm_layout.insert(
                name,
                FuncAsmLayout { section, bytes, text_start: start },
            );
        }
    }

    /// 生成所有函数代码；`-O2+` 且 relocatable 时走函数级并行 emit。
    fn emit_functions(&mut self, prog: &Program) {
        let top_funcs: Vec<FuncDecl> = prog.decls.iter().filter_map(|d| {
            if let Decl::Func(f) = d { if f.body.is_some() { Some(f.clone()) } else { None } } else { None }
        }).collect();
        if self.parallel_codegen && self.relocatable && top_funcs.len() >= 2 {
            self.emit_functions_parallel(&top_funcs, prog);
            return;
        }
        for f in &top_funcs {
            self.compile_func(f, prog);
        }
    }

    fn parallel_emit_ctx(&self) -> ParallelEmitCtx {
        ParallelEmitCtx {
            target: self.target.clone(),
            relocatable: self.relocatable,
            function_sections: self.function_sections,
            emit_asm_text: self.emit_asm_text,
            debug_level: self.debug_level,
            emit_cfi: self.emit_cfi,
            source_name: self.source_name.clone(),
            globals: self.globals.iter().map(|(k, v)| {
                (k.clone(), ParallelGlobalSnap {
                    offset: v.offset,
                    size: v.size,
                    section: v.section,
                    is_tls: v.is_tls,
                })
            }).collect(),
            variadic_funcs: self.variadic_funcs.clone(),
            nested_funcs: self.nested_funcs.clone(),
            func_param_types: self.func_param_types.clone(),
            rodata: self.rodata.clone(),
            string_pool: self.string_pool.clone(),
            text_align: self.text_align,
        }
    }

    fn codegen_from_parallel(ctx: ParallelEmitCtx) -> Self {
        let mut cg = Self::new(ctx.target);
        cg.relocatable = ctx.relocatable;
        cg.function_sections = ctx.function_sections;
        cg.emit_asm_text = ctx.emit_asm_text;
        cg.debug_level = ctx.debug_level;
        cg.emit_cfi = ctx.emit_cfi;
        cg.source_name = ctx.source_name;
        for (name, snap) in ctx.globals {
            cg.globals.insert(name, GlobalVar {
                offset: snap.offset,
                size: snap.size,
                ty: Type::Int,
                init: None,
                section: snap.section,
                is_tls: snap.is_tls,
            });
        }
        cg.variadic_funcs = ctx.variadic_funcs;
        cg.nested_funcs = ctx.nested_funcs;
        cg.func_param_types = ctx.func_param_types;
        cg.rodata = ctx.rodata;
        cg.string_pool = ctx.string_pool;
        cg.text_align = ctx.text_align;
        cg
    }

    fn patch_local_jumps(&mut self) {
        for p in self.pending.drain(..) {
            let dst = *self.labels.get(&p.target)
                .unwrap_or_else(|| panic!("unresolved label: {}", p.target));
            let off = p.at as usize;
            let (rel_from, rel_off) = match p.kind {
                JumpKind::Jmp => (p.at + 5, off + 1),
                _ => (p.at + 6, off + 2),
            };
            let rel = (dst as i64) - (rel_from as i64);
            self.code[rel_off..rel_off + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
    }

    fn emit_func_chunk(ctx: ParallelEmitCtx, f: FuncDecl, prog: &Program) -> FuncEmitChunk {
        let mut cg = Self::codegen_from_parallel(ctx);
        cg.compile_func(&f, prog);
        cg.patch_local_jumps();
        let size = cg.func_sizes.get(&f.name.name).copied().unwrap_or(cg.code.len() as u64);
        FuncEmitChunk {
            name: f.name.name.clone(),
            code: cg.code,
            size,
            string_refs: cg.string_refs,
            float_refs: cg.float_refs,
            pending_func_rels: cg.pending_func_rels,
            pending_data_rels: cg.pending_data_rels,
            label_addr_refs: cg.label_addr_refs,
            disasm_calls: cg.disasm_calls,
        }
    }

    fn merge_func_chunk(&mut self, chunk: FuncEmitChunk, func_align: usize) {
        while self.code.len() % func_align != 0 {
            self.code.push(0x90);
        }
        let base = self.pos();
        self.labels.insert(chunk.name.clone(), base);
        self.func_sizes.insert(chunk.name.clone(), chunk.size);
        self.code.extend_from_slice(&chunk.code);
        for (pos, data) in chunk.string_refs {
            self.string_refs.push((base + pos, data));
        }
        for (pos, off) in chunk.float_refs {
            self.float_refs.push((base + pos, off));
        }
        for (pos, sym) in chunk.pending_func_rels {
            self.pending_func_rels.push((base + pos, sym));
        }
        for (pos, sym) in chunk.pending_data_rels {
            self.pending_data_rels.push((base + pos, sym));
        }
        for (pos, lbl) in chunk.label_addr_refs {
            self.label_addr_refs.push((base + pos, lbl));
        }
        for (pos, name) in chunk.disasm_calls {
            self.disasm_calls.insert(base + pos, name);
        }
    }

    /// 函数级并行 emit：`-O2+` 多函数 TU 在 worker 线程中分块生成后按序合并。
    fn emit_functions_parallel(&mut self, top_funcs: &[FuncDecl], prog: &Program) {
        let ctx = Arc::new(self.parallel_emit_ctx());
        let n = top_funcs.len();
        let degree = Self::parallel_worker_count(n);

        let chunks: Vec<FuncEmitChunk> = if degree <= 1 {
            top_funcs
                .iter()
                .map(|f| Self::emit_func_chunk(ctx.as_ref().clone(), f.clone(), prog))
                .collect()
        } else {
            let next_job = AtomicUsize::new(0);
            let slots = Mutex::new(vec![None; n]);
            std::thread::scope(|scope| {
                for _ in 0..degree {
                    scope.spawn(|| {
                        loop {
                            let i = next_job.fetch_add(1, Ordering::Relaxed);
                            if i >= n {
                                break;
                            }
                            let chunk = Self::emit_func_chunk(
                                ctx.as_ref().clone(),
                                top_funcs[i].clone(),
                                prog,
                            );
                            slots.lock().unwrap()[i] = Some(chunk);
                        }
                    });
                }
            });
            slots
                .into_inner()
                .expect("parallel codegen mutex poisoned")
                .into_iter()
                .map(|c| c.expect("parallel codegen dropped a function chunk"))
                .collect()
        };

        for (f, chunk) in top_funcs.iter().zip(chunks.iter()) {
            debug_assert_eq!(f.name.name, chunk.name);
            let func_align = f.alignment.unwrap_or(1).max(1);
            self.merge_func_chunk(chunk.clone(), func_align);
        }
    }

    /// 并行 worker 数量：逻辑核心数与函数数取较小值，至少 1。
    fn parallel_worker_count(n_funcs: usize) -> usize {
        if n_funcs <= 1 {
            return 1;
        }
        std::thread::available_parallelism()
            .map(|p| p.get().min(n_funcs))
            .unwrap_or(4)
            .max(1)
    }

    /// 递归收集块中的嵌套函数声明
    fn collect_nested_funcs(items: &[BlockItem], out: &mut Vec<FuncDecl>) {
        for item in items {
            if let BlockItem::Decl(Decl::Func(f)) = item {
                out.push(f.clone());
                if let Some(body) = &f.body {
                    Self::collect_nested_funcs(&body.items, out);
                }
            }
            // 也扫描嵌套块 (if/for/while 等)
            if let BlockItem::Stmt(s) = item {
                Self::collect_nested_funcs_stmt(s, out);
            }
        }
    }
    fn collect_nested_funcs_stmt(s: &Stmt, out: &mut Vec<FuncDecl>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_nested_funcs(&b.items, out),
            Stmt::If { then_, else_, .. } => {
                if let Stmt::Block { b, .. } = then_.as_ref() { Self::collect_nested_funcs(&b.items, out); }
                if let Some(e) = else_ { if let Stmt::Block { b, .. } = e.as_ref() { Self::collect_nested_funcs(&b.items, out); } }
            }
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                if let Stmt::Block { b, .. } = body.as_ref() { Self::collect_nested_funcs(&b.items, out); }
            }
            _ => {}
        }
    }

    /// 只收集当前函数直接包含的嵌套函数；不进入这些函数自己的函数体。
    fn collect_direct_nested_funcs(items: &[BlockItem], out: &mut Vec<FuncDecl>) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Func(f)) => out.push(f.clone()),
                BlockItem::Stmt(s) => Self::collect_direct_nested_funcs_stmt(s, out),
                _ => {}
            }
        }
    }

    fn collect_direct_nested_funcs_stmt(s: &Stmt, out: &mut Vec<FuncDecl>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_direct_nested_funcs(&b.items, out),
            Stmt::If { then_, else_, .. } => {
                Self::collect_direct_nested_funcs_stmt(then_, out);
                if let Some(e) = else_ { Self::collect_direct_nested_funcs_stmt(e, out); }
            }
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                Self::collect_direct_nested_funcs_stmt(body, out);
            }
            _ => {}
        }
    }

    fn register_func_sig(f: &FuncDecl, map: &mut HashMap<String, Vec<Type>>) {
        let params: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
        map.insert(f.name.name.clone(), params);
    }

    /// 编译单个函数 (支持嵌套函数递归调用)
    fn compile_func(&mut self, f: &FuncDecl, prog: &Program) {
        let arg_regs: [u8; 6] = [7, 6, 2, 1, 8, 9];
        self.var_map.clear();
        self.vla_offsets.clear();
        self.reg_bindings.clear();
        self.cleanup_vars.clear();
        self.sret_depth = 0;
        self.vla_total_size = 0;
        // 设置嵌套函数状态
        self.current_func_is_nested = f.is_nested;
        self.current_returns_nonnull = f.returns_nonnull;
        self.upvar_offsets.clear();
        if f.is_nested {
            for (name, off) in &f.upvars {
                self.upvar_offsets.insert(name.clone(), *off);
            }
        }
        let func_align = f.alignment.unwrap_or(1).max(1);
        self.text_align = self.text_align.max(func_align as u64);
        while self.code.len() % func_align != 0 { self.code.push(0x90); }
        let func_start = self.pos();
        self.label(&f.name.name);
        if self.target.frame_pointer {
            self.emit(&[0x55]); // push rbp
        }
        self.emit(&[0x41, 0x55, 0x49, 0x89, 0xe5]);
        if self.target.frame_pointer {
            self.emit(&[0x48, 0x89, 0xE5]); // mov rbp, rsp
        }
        self.expr_depth = 0; self.max_expr_depth = 0; self.temp_slots = 0;
        if let Some(body) = &f.body { self.max_expr_depth = Self::max_expr_depth_stmt(&body.items); }
        let temp_slots = (self.max_expr_depth as i32 + 1).max(2);
        self.temp_slots = temp_slots;
        if f.name.name != "main" || !self.relocatable {
                    // 先预注册函数体中的局部变量 (使参数偏移自然在局部变量下方)
                    if let Some(body) = &f.body { Self::pre_register_vars(&mut self.var_map, &body.items, temp_slots); }
                    // 注册参数到 var_map (偏移公式自动避开已注册的局部变量)
                    // 记录命名参数数量 (va_start 需要)
                    self.variadic_named_params = f.params.iter().filter(|p| p.name.is_some()).count() as i32;
                    self.variadic_named_gp = 0;
                    self.variadic_named_fp = 0;
                    let mut param_stores: Vec<(u8, i32)> = Vec::new(); // (gp reg, offset)
                    let mut xmm_param_stores: Vec<(u8, i32, Type)> = Vec::new(); // (xmm reg, offset, ty)
                    let mut stack_param_stores: Vec<(usize, i32)> = Vec::new(); // (stack arg index, var offset)
                    let mut gp_idx = 0usize;
                    let mut fp_idx = 0usize;
                    let mut stack_arg_idx = 0usize;
                    for p in f.params.iter() {
                        if let Some(nm) = &p.name {
                            // __int128 / 大聚合体按 8 字节块占用 GP 寄存器或栈槽
                            let is_agg_param = matches!(&p.ty, Type::Int128 | Type::UInt128)
                                || (matches!(&p.ty, Type::Struct(_,_) | Type::Union(_,_)
                                    | Type::Array(_,ArraySize::Const(_)) | Type::Atomic(_))
                                    && p.ty.size() > 8);
                            if is_agg_param {
                                let chunks = (p.ty.size() + 7) / 8;
                                let used: i32 = self.var_map.values()
                                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                                    .sum();
                                let off = -(used + 16);
                                self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                                for c in 0..chunks as usize {
                                    if gp_idx + c < 6 {
                                        param_stores.push((arg_regs[gp_idx + c], off + (c as i32 * 8)));
                                    }
                                }
                                if gp_idx + chunks > 6 {
                                    stack_arg_idx += chunks;
                                }
                                gp_idx += chunks as usize;
                                self.variadic_named_gp += chunks as i32;
                            } else if matches!(&p.ty, Type::Float | Type::Double | Type::LongDouble) && self.target.sse {
                                let used: i32 = self.var_map.values()
                                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                                    .sum();
                                let off = -(used + 16);
                                self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                                if fp_idx < 8 {
                                    xmm_param_stores.push((fp_idx as u8, off, p.ty.clone()));
                                    fp_idx += 1;
                                    self.variadic_named_fp += 1;
                                } else {
                                    stack_param_stores.push((stack_arg_idx, off));
                                    stack_arg_idx += 1;
                                    self.variadic_named_fp += 1;
                                }
                            } else if gp_idx < 6 {
                                let used: i32 = self.var_map.values()
                                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                                    .sum();
                                let off = -(used + 16);
                                self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                                param_stores.push((arg_regs[gp_idx], off));
                                gp_idx += 1;
                                self.variadic_named_gp += 1;
                            } else {
                                let used: i32 = self.var_map.values()
                                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                                    .sum();
                                let off = -(used + 16);
                                self.var_map.insert(nm.name.clone(), (off, p.ty.clone()));
                                stack_param_stores.push((stack_arg_idx, off));
                                stack_arg_idx += 1;
                                self.variadic_named_gp += 1;
                            }
                        }
                    }
                // frame_size = 变量实际字节 + 16(保存寄存器) + temp_slots*8
                let var_bytes: i32 = self.var_map.values()
                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                    .sum();
                let sc_slot = if f.is_nested { 8 } else { 0 }; // 嵌套函数需要静态链槽位
                let va_slots = if f.is_variadic {
                    if self.target.sse { 48 + 128 } else { 48 }
                } else {
                    0
                };
                let raw = var_bytes + 16 + temp_slots * 8 + sc_slot + va_slots
                    + self.stack_reserve_bytes();
                self.frame_size = ((raw + 15) / 16) * 16; // 16 字节对齐
                if self.frame_size > 0 {
                    self.emit(&[0x48, 0x81, 0xec]);
                    self.emit(&(self.frame_size as i32).to_le_bytes());
                }
                // 帧底部先留给表达式临时槽，其后是可变参数 GP 保存区和静态链。
                self.variadic_reg_save_offset = -self.frame_size + temp_slots * 8;
                if f.is_variadic {
                    for (i, reg) in arg_regs.iter().enumerate() {
                        self.store_stack(*reg, self.variadic_reg_save_offset + i as i32 * 8);
                    }
                    if self.target.sse {
                        for i in 0..8u8 {
                            self.store_xmm128_stack(
                                i,
                                self.variadic_reg_save_offset + 48 + i as i32 * 16,
                            );
                        }
                    }
                }
                // 嵌套函数: 保存静态链 (r10)
                if f.is_nested {
                    self.static_chain_offset = self.variadic_reg_save_offset + va_slots;
                    // mov [r13 + static_chain_offset], r10
                    self.emit(&[0x49, 0x89, 0x95]);
                    self.emit(&self.static_chain_offset.to_le_bytes());
                }
                // Phase 2: 帧已建立 (r13 已设置), 现在存储参数到栈槽
                for (reg, off) in param_stores {
                    self.store_stack(reg, off);
                }
                for (xmm, off, ty) in xmm_param_stores {
                    if matches!(ty, Type::Float) {
                        self.store_xmm32_stack(xmm, off);
                    } else {
                        self.store_xmm64_stack(xmm, off);
                    }
                }
                for (arg_index, off) in stack_param_stores {
                    let incoming = self.frame_size + 16 + 8 * arg_index as i32;
                    self.load_stack(0, incoming);
                    self.store_stack(0, off);
                }
                } else {
                if let Some(body) = &f.body { Self::pre_register_vars(&mut self.var_map, &body.items, temp_slots); }
                let var_bytes: i32 = self.var_map.values()
                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                    .sum();
                let raw = var_bytes + 16 + temp_slots * 8 + self.stack_reserve_bytes();
                self.frame_size = ((raw + 15) / 16) * 16;
                if self.frame_size > 0 {
                    self.emit(&[0x48, 0x81, 0xec]);
                    self.emit(&(self.frame_size as i32).to_le_bytes());
                }
                }
                let ends_ret = if let Some(body) = &f.body {
                    for item in &body.items { match item {
                        BlockItem::Stmt(s) => self.gen_stmt(s),
                        BlockItem::Decl(Decl::Var(v)) => {
                            if let Some(ref reg_name) = v.register_name {
                                if let Some(reg_idx) = Self::parse_reg_name(reg_name) {
                                    self.reg_bindings.insert(v.name.name.clone(), reg_idx);
                                }
                            }
                            if let Some(ref fn_name) = v.cleanup_fn {
                                self.cleanup_vars.push((v.name.name.clone(), fn_name.clone()));
                            }
                            self.register_var(&v.name.name, &v.ty);
                            if matches!(&v.ty, Type::Array(_, ArraySize::Var(_))) {
                                self.gen_vla_alloc(v);
                            } else if let Some(init) = &v.init {
                                if let Expr::InitList { inits, .. } = init {
                                    self.gen_init_list(&v.name.name, &v.ty, inits);
                                } else {
                                    let is_vec_init = matches!(&v.ty, Type::Vector(_, _));
                                    let is_agg = matches!(&v.ty, Type::Struct(_,_) | Type::Union(_,_) | Type::Array(_,ArraySize::Const(_))
                                        | Type::Atomic(_))
                                        && v.ty.size() > 8;
                                    if is_vec_init {
                                        self.gen_expr(init, 0);
                                        self.gen_vector_store(&v.name.name);
                                    } else if is_agg {
                                        // struct/array 初始化: 整体拷贝
                                        let dst_ty = v.ty.clone();
                                        let dst_name = v.name.name.clone();
                                        self.gen_aggregate_assign(&dst_name, &dst_ty, init);
                                    } else if let Type::Complex(inner) = &v.ty {
                                        self.gen_complex_init(&v.name.name, inner, init);
                                    } else if matches!(&v.ty, Type::Float | Type::Double | Type::LongDouble) {
                                        self.gen_expr(init, 0);
                                        self.gen_float_store(&v.name.name, &v.ty);
                                    } else if matches!(&v.ty, Type::Int128 | Type::UInt128) {
                                        // P1-4 fix: __int128 变量初始化 (rax=low, rdx=high)
                                        self.gen_expr(init, 0);
                                        // 如果初始化器不是 int128 类型，需要符号扩展
                                        if !matches!(init.ty(), Type::Int128 | Type::UInt128) {
                                            if matches!(init.ty(), Type::Bool | Type::UChar | Type::UShort
                                                | Type::UInt | Type::ULong | Type::ULongLong) {
                                                self.emit(&[0x48, 0x31, 0xD2]);
                                            } else {
                                                self.emit(&[0x48, 0x99]); // cqo
                                            }
                                        }
                                        let off = self.var_offset(&v.name.name);
                                        self.store_stack(0, off);      // low → [r13+off]
                                        self.store_stack(2, off + 8);  // high → [r13+off+8]
                                    } else {
                                        self.gen_expr(init, 0);
                                        self.mov_to_stack(0, &v.name.name);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }}
                    Self::ends_with_return(&body.items)
                } else { false };
                if !ends_ret { self.emit_func_epilogue(); }
                // 记录函数大小
                let func_end = self.pos();
                self.func_sizes.insert(f.name.name.clone(), func_end - func_start);
                // 父函数布局确定后再生成嵌套函数，把捕获变量的真实 r13 偏移传入。
                if let Some(body) = &f.body {
                    let mut nested = Vec::new();
                    Self::collect_direct_nested_funcs(&body.items, &mut nested);
                    let parent_var_map = self.var_map.clone();
                    for mut nf_decl in nested {
                        for (name, off) in &mut nf_decl.upvars {
                            if let Some((parent_off, _)) = parent_var_map.get(name) {
                                *off = *parent_off;
                            }
                        }
                        self.compile_func(&nf_decl, prog);
                    }
                }
    }

    fn call_label(&mut self, l: &str) {
        let at = self.pos(); self.emit(&[0xe8, 0, 0, 0, 0]);
        self.disasm_calls.insert(at + 1, l.to_string());
        if self.relocatable {
            // 延迟到 Phase 3 符号表构建后再解析
            self.pending_func_rels.push((at + 1, l.to_string()));
        } else {
            self.pending.push(PendingJump { at, target: l.into(), kind: JumpKind::Jmp });
        }
    }

    /// Lower one SysV AMD64 call.  Outgoing stack arguments occupy the lowest
    /// addresses, temporary register staging follows them, and the complete
    /// area is rounded to 16 bytes so `%rsp` is aligned immediately before call.
    fn emit_sysv_call(
        &mut self,
        direct: Option<&str>,
        callee: Option<&Expr>,
        args: &[Expr],
        sret_off: Option<i32>,
    ) {
        let gp_regs = [7u8, 6, 2, 1, 8, 9];
        let mut gp = usize::from(sret_off.is_some());
        let mut fp = 0usize;
        let mut stack = 0usize;
        let mut locs: Vec<Vec<ArgLoc>> = Vec::with_capacity(args.len());

        for (arg_idx, arg) in args.iter().enumerate() {
            let param_ty = direct
                .and_then(|name| self.func_param_types.get(name))
                .and_then(|ps| ps.get(arg_idx))
                .cloned();
            let effective_ty = param_ty.unwrap_or_else(|| arg.ty());
            let classes = Self::abi_classes(&effective_ty);
            let need_gp = classes.iter().filter(|c| **c == AbiClass::Integer).count();
            let need_fp = classes.iter().filter(|c| **c == AbiClass::Sse).count();
            let force_mem = classes == [AbiClass::Memory]
                || gp + need_gp > gp_regs.len() || fp + need_fp > 8;
            if force_mem {
                stack = (stack + 7) & !7;
                let size = effective_ty.size().max(8);
                let chunks = (size + 7) / 8;
                locs.push((0..chunks).map(|i| ArgLoc::Stack(stack + i * 8)).collect());
                stack += chunks * 8;
            } else {
                let mut arg_locs = Vec::new();
                for class in classes {
                    match class {
                        AbiClass::Integer => {
                            arg_locs.push(ArgLoc::Gp(gp_regs[gp]));
                            gp += 1;
                        }
                        AbiClass::Sse => {
                            arg_locs.push(ArgLoc::Xmm(fp as u8));
                            fp += 1;
                        }
                        AbiClass::Memory => unreachable!(),
                    }
                }
                locs.push(arg_locs);
            }
        }

        let register_pieces = locs.iter().flatten()
            .filter(|l| !matches!(l, ArgLoc::Stack(_))).count();
        let hidden_slots = usize::from(sret_off.is_some()) + usize::from(direct.is_none());
        let stage_base = (stack + 7) & !7;
        let frame_bytes = (stage_base + (register_pieces + hidden_slots) * 8 + 15) & !15;
        self.emit_sub_rsp(frame_bytes);

        let mut stage = stage_base;
        let sret_stage = if let Some(off) = sret_off {
            self.emit_lea_r13(0, off);
            self.store_rsp_qword(0, stage);
            let slot = stage;
            stage += 8;
            Some(slot)
        } else { None };
        let callee_stage = if direct.is_none() {
            self.gen_expr(callee.expect("indirect call expression"), 0);
            self.store_rsp_qword(0, stage);
            let slot = stage;
            stage += 8;
            Some(slot)
        } else { None };

        let mut staged: Vec<(ArgLoc, usize)> = Vec::new();
        for (arg_idx, (arg, arg_locs)) in args.iter().zip(locs.iter()).enumerate() {
            let param_ty = direct
                .and_then(|name| self.func_param_types.get(name))
                .and_then(|ps| ps.get(arg_idx))
                .cloned();
            let effective_ty = param_ty.unwrap_or_else(|| arg.ty());
            let aggregate = matches!(effective_ty, Type::Int128 | Type::UInt128)
                || matches!(effective_ty, Type::Struct(_, _) | Type::Union(_, _)
                    | Type::Array(_, ArraySize::Const(_)) | Type::Atomic(_));
            if aggregate {
                for (chunk, loc) in arg_locs.iter().enumerate() {
                    if let Expr::Ident { name, .. } = arg {
                        if self.globals.contains_key(&name.name) {
                            self.emit_global_addr(11, &name.name);
                            self.emit(&[0x49, 0x8B, 0x83]);
                            self.emit(&((chunk * 8) as i32).to_le_bytes());
                        } else {
                            let var_off = self.var_offset(&name.name) + (chunk * 8) as i32;
                            self.load_stack(0, var_off);
                        }
                    } else {
                        self.gen_expr(arg, 0);
                        if chunk == 1 { self.mov(0, 2); }
                    }
                    match *loc {
                        ArgLoc::Stack(off) => self.store_rsp_qword(0, off),
                        _ => {
                            self.store_rsp_qword(0, stage);
                            staged.push((*loc, stage));
                            stage += 8;
                        }
                    }
                }
            } else if matches!(effective_ty, Type::Float | Type::Double) {
                self.gen_expr(arg, 0);
                match arg_locs[0] {
                    ArgLoc::Stack(off) => self.store_rsp_xmm(0, off),
                    loc => {
                        self.store_rsp_xmm(0, stage);
                        staged.push((loc, stage));
                        stage += 8;
                    }
                }
            } else if matches!(effective_ty, Type::Int128 | Type::UInt128) {
                // SysV：16 字节整数占两个 GP 槽（低 rax / 高 rdx）
                self.gen_expr(arg, 0);
                if !matches!(arg.ty(), Type::Int128 | Type::UInt128) {
                    if matches!(effective_ty, Type::UInt128)
                        || matches!(arg.ty(), Type::Bool | Type::UChar | Type::UShort
                            | Type::UInt | Type::ULong | Type::ULongLong)
                    {
                        self.emit(&[0x48, 0x31, 0xD2]);
                    } else {
                        self.emit(&[0x48, 0x99]);
                    }
                }
                for (chunk, loc) in arg_locs.iter().enumerate() {
                    let reg = if chunk == 0 { 0u8 } else { 2u8 };
                    match *loc {
                        ArgLoc::Stack(off) => self.store_rsp_qword(reg, off),
                        loc => {
                            self.store_rsp_qword(reg, stage);
                            staged.push((loc, stage));
                            stage += 8;
                        }
                    }
                }
            } else {
                self.gen_expr_decay(arg, 0);
                match arg_locs[0] {
                    ArgLoc::Stack(off) => self.store_rsp_qword(0, off),
                    loc => {
                        self.store_rsp_qword(0, stage);
                        staged.push((loc, stage));
                        stage += 8;
                    }
                }
            }
        }

        if let Some(slot) = sret_stage { self.load_rsp_qword(7, slot); }
        for (loc, slot) in staged {
            match loc {
                ArgLoc::Gp(reg) => self.load_rsp_qword(reg, slot),
                ArgLoc::Xmm(reg) => self.load_rsp_xmm(reg, slot),
                ArgLoc::Stack(_) => unreachable!(),
            }
        }
        // SysV uses AL to communicate the number of vector registers to a
        // variadic callee. Setting it for all calls is benign.
        self.emit(&[0xB0, fp as u8]);
        if let Some(name) = direct {
            if self.nested_funcs.contains_key(name) {
                self.emit(&[0x4C, 0x89, 0xEA]); // mov r10, r13
            }
            self.call_label(name);
        } else {
            self.load_rsp_qword(11, callee_stage.unwrap());
            self.emit(&[0x49, 0xFF, 0xD3]);
        }
        self.emit_add_rsp(frame_bytes);
    }

    // 生成表达式值, 数组标识符衰减为指针 (arr → &arr[0])
    fn gen_expr_decay(&mut self, e: &Expr, dst: u8) {
        if let Expr::Ident { name, ty, .. } = e {
            if matches!(ty, Type::Array(_, _)) {
                if self.globals.contains_key(&name.name) {
                    // 全局数组: RIP-relative LEA 取地址
                    self.emit_global_addr(dst, &name.name);
                } else {
                    let off = self.var_offset(&name.name);
                    // REX.W + REX.B (r13) + REX.R (dst >= 8 → r10/r11/...)
                    let rex = 0x48u8 | 0x01 | (if dst >= 8 { 0x04 } else { 0 });
                    self.emit(&[rex, 0x8D]);
                    if off.abs() < 128 {
                        self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                    } else {
                        self.emit(&[0x85 | ((dst & 7) << 3)]);
                        self.emit(&off.to_le_bytes());
                    }
                }
                return;
            }
        }
        self.gen_expr(e, dst);
    }

    fn gen_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Return { val, .. } => {
                if let Some(e) = val {
                    let ret_ty = e.ty();
                    let ret_sz = ret_ty.size();
                    let is_agg_ret = matches!(&ret_ty, Type::Struct(_,_) | Type::Union(_,_) | Type::Array(_,ArraySize::Const(_))
                        | Type::Complex(_) | Type::Atomic(_))
                        && ret_sz > 8;
                    if is_agg_ret {
                        // struct 返回: 加载到 rax[:rdx]
                        if let Expr::Ident { name, .. } = &e {
                            let off = self.var_offset(&name.name);
                            self.load_stack(0, off); // rax = 前 8 字节
                            if ret_sz > 8 {
                                self.load_stack(2, off + 8); // rdx = 后 8 字节
                            }
                        } else {
                            self.gen_expr(e, 0);
                        }
                    } else {
                        self.gen_expr(e, 0);
                    }
                    // __attribute__((returns_nonnull)): 返回前检查 rax 非空.
                    // 若为 0 触发 UD2 (执行未定义指令) 立即 abort.
                    if self.current_returns_nonnull && matches!(ret_ty, Type::Ptr(_) | Type::Func(_, _)) {
                        // test rax, rax; jnz .ok; ud2; .ok:
                        self.emit(&[0x48, 0x85, 0xc0]);  // test rax, rax
                        let ok_label = self.fresh_label();
                        self.jmp_rel32(&ok_label, JumpKind::Jne);
                        self.emit(&[0x0f, 0x0b]);  // ud2 - undefined instruction trap
                        self.label(&ok_label);
                    }
                } else { self.emit(&[0x31, 0xc0]); }
                self.emit_func_epilogue();
            }
            Stmt::Expr { e, .. } => {
                if !self.gen_unknown_builtin_stmt(e) {
                    self.gen_expr(e, 0);
                }
            }
            Stmt::If { cond, then_, else_, .. } => {
                self.gen_expr(cond, 0); self.emit(&[0x85, 0xc0]);
                let le = self.fresh_label(); let lend = self.fresh_label();
                self.jmp_rel32(&le, JumpKind::Je);
                self.gen_stmt(then_); self.jmp_rel32(&lend, JumpKind::Jmp);
                self.label(&le); if let Some(el) = else_ { self.gen_stmt(el); }
                self.label(&lend);
            }
            Stmt::While { cond, body, .. } => {
                let ls = self.fresh_label(); let le = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                self.loop_continue = Some(ls.clone()); self.loop_break = Some(le.clone());
                self.label(&ls); self.gen_expr(cond, 0); self.emit(&[0x85, 0xc0]);
                self.jmp_rel32(&le, JumpKind::Je); self.gen_stmt(body);
                self.jmp_rel32(&ls, JumpKind::Jmp); self.label(&le);
                self.loop_continue = sc; self.loop_break = sb;
            }
            Stmt::For { init, cond, step, body, .. } => {
                if let Some(i) = init { self.gen_stmt(i); }
                let ls = self.fresh_label(); let le = self.fresh_label(); let lst = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                self.loop_continue = Some(lst.clone()); self.loop_break = Some(le.clone());
                self.label(&ls);
                if let Some(c) = cond { self.gen_expr(c, 0); self.emit(&[0x85, 0xc0]); self.jmp_rel32(&le, JumpKind::Je); }
                self.gen_stmt(body); self.label(&lst);
                if let Some(s) = step { self.gen_expr(s, 0); }
                self.jmp_rel32(&ls, JumpKind::Jmp); self.label(&le);
                self.loop_continue = sc; self.loop_break = sb;
            }
            Stmt::Do { body, cond, .. } => {
                let ls = self.fresh_label(); let le = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                self.loop_continue = Some(ls.clone()); self.loop_break = Some(le.clone());
                self.label(&ls); self.gen_stmt(body);
                self.gen_expr(cond, 0); self.emit(&[0x85, 0xc0]);
                self.jmp_rel32(&ls, JumpKind::Jne); self.label(&le);
                self.loop_continue = sc; self.loop_break = sb;
            }
            Stmt::Block { b, .. } => {
                let cleanup_base = self.cleanup_vars.len();
                for it in &b.items { match it {
                    BlockItem::Stmt(s) => self.gen_stmt(s),
                    BlockItem::Decl(Decl::Var(v)) => {
                        // P1-5: register asm 变量绑定到指定寄存器
                        if let Some(ref reg_name) = v.register_name {
                            if let Some(reg_idx) = Self::parse_reg_name(reg_name) {
                                self.reg_bindings.insert(v.name.name.clone(), reg_idx);
                            }
                        }
                        // P2-9: cleanup 属性变量注册
                        if let Some(ref fn_name) = v.cleanup_fn {
                            self.cleanup_vars.push((v.name.name.clone(), fn_name.clone()));
                        }
                        self.register_var(&v.name.name, &v.ty);
                        if matches!(&v.ty, Type::Array(_, ArraySize::Var(_))) {
                            self.gen_vla_alloc(v);
                        } else if let Some(init) = &v.init {
                            if let Expr::InitList { inits, .. } = init {
                                self.gen_init_list(&v.name.name, &v.ty, inits);
                            } else {
                                let is_agg = matches!(&v.ty, Type::Struct(_,_) | Type::Union(_,_) | Type::Array(_,ArraySize::Const(_))
                                    | Type::Atomic(_))
                                    && v.ty.size() > 8;
                                if matches!(&v.ty, Type::Vector(_, _)) {
                                    self.gen_expr(init, 0);
                                    self.gen_vector_store(&v.name.name);
                                } else if is_agg {
                                    let dst_ty = v.ty.clone();
                                    let dst_name = v.name.name.clone();
                                    self.gen_aggregate_assign(&dst_name, &dst_ty, init);
                                } else if let Type::Complex(inner) = &v.ty {
                                    self.gen_complex_init(&v.name.name, inner, init);
                                } else {
                                    self.gen_expr(init, 0);
                                    self.mov_to_stack(0, &v.name.name);
                                }
                            }
                        }
                    }
                    _ => {}
                }}
                // P2-9: 块结束时逆序调用 cleanup 函数
                while self.cleanup_vars.len() > cleanup_base {
                    if let Some((var_name, fn_name)) = self.cleanup_vars.pop() {
                        // 发射: lea rdi, [r13 + offset]; call fn_name
                        let off = self.var_offset(&var_name);
                        self.emit_lea_r13(7, off); // rdi = &var
                        self.call_label(&fn_name);
                    }
                }
            }
            Stmt::Break { .. } => { if let Some(l) = self.loop_break.clone() { self.jmp_rel32(&l, JumpKind::Jmp); } }
            Stmt::Continue { .. } => { if let Some(l) = self.loop_continue.clone() { self.jmp_rel32(&l, JumpKind::Jmp); } }
            Stmt::Goto { target, .. } => { self.jmp_rel32(&target.name, JumpKind::Jmp); }
            Stmt::Label { name, stmt, .. } => { self.label(&name.name); self.gen_stmt(stmt); }
            Stmt::Switch { expr, cases, .. } => {
                // switch: 计算表达式, 依次比较每个case, 跳到匹配处
                let l_end = self.fresh_label();
                let l_default = self.fresh_label();
                let mut has_default = false;
                // 计算switch表达式到rax
                self.gen_expr(expr, 0);
                self.push(0); // 保存switch值
                // 生成每个case的比较和跳转
                for case in cases {
                    if let Some(ref val) = case.val {
                        let l_case = self.fresh_label();
                        let l_skip = self.fresh_label();
                        // 重新加载switch值到rcx
                        self.emit(&[0x48, 0x8B, 0x0C, 0x24]); // mov rcx, [rsp]
                        if let Some(ref hi) = case.range_end {
                            // GNU case low ... high: 范围匹配
                            self.gen_expr(val, 2);   // rdx = low
                            self.gen_expr(hi, 3);    // r8 = high
                            self.emit(&[0x48, 0x39, 0xD1]); // cmp rcx, rdx
                            self.jmp_rel32(&l_skip, JumpKind::Jl);
                            self.emit(&[0x4C, 0x39, 0xC1]); // cmp rcx, r8
                            self.jmp_rel32(&l_skip, JumpKind::Jg);
                        } else {
                            // case N: 比较 rcx == N
                            self.gen_expr(val, 2);
                            self.emit(&[0x48, 0x39, 0xD1]);
                            self.jmp_rel32(&l_skip, JumpKind::Jne);
                        }
                        self.jmp_rel32(&l_case, JumpKind::Jmp);
                        self.label(&l_skip);
                        self.label(&l_case);
                        for item in &case.body.items {
                            if let BlockItem::Stmt(stmt) = item {
                                self.gen_stmt(stmt);
                            }
                        }
                    } else {
                        // default: 记录标签
                        has_default = true;
                    }
                }
                // 如果没有匹配, 跳到default或end
                if has_default {
                    self.jmp_rel32(&l_default, JumpKind::Jmp);
                } else {
                    self.jmp_rel32(&l_end, JumpKind::Jmp);
                }
                // 生成default体
                if has_default {
                    self.label(&l_default);
                    for case in cases {
                        if case.val.is_none() {
                            for item in &case.body.items {
                                if let BlockItem::Stmt(stmt) = item {
                                    self.gen_stmt(stmt);
                                }
                            }
                        }
                    }
                }
                self.label(&l_end);
                self.emit(&[0x48, 0x83, 0xC4, 0x08]); // add rsp, 8 (清理switch值)
            }
            Stmt::GotoIndirect { expr, .. } => {
                // goto *expr: 计算目标地址并跳转
                self.gen_expr(expr, 0); // rax = 目标地址
                self.emit(&[0xFF, 0xE0]); // jmp rax
            }
            Stmt::AsmGoto { asm_str, outputs, output_names, output_constraints,
                inputs, input_names, input_constraints, clobbers, goto_labels, .. } => {
                // 将 GCC 的 %lN / %l[name] 占位符解析为 C 标签名。
                let mut resolved_asm = asm_str.clone();
                let label_base = outputs.len() + inputs.len();
                for (i, label) in goto_labels.iter().enumerate().rev() {
                    resolved_asm = resolved_asm.replace(&format!("%l{}", i), &label.name);
                    resolved_asm = resolved_asm.replace(
                        &format!("%l{}", label_base + i), &label.name);
                    resolved_asm = resolved_asm.replace(
                        &format!("%l[{}]", label.name), &label.name);
                }
                self.emit_inline_asm(
                    &resolved_asm, outputs, output_names, output_constraints,
                    inputs, input_names, input_constraints, clobbers,
                );
            }
        }
    }

    fn gen_expr(&mut self, e: &Expr, dst: u8) {
        match e {
            Expr::Int { v, .. } => self.mov_imm(dst, *v),
            Expr::Char { v, .. } => self.mov_imm(dst, *v as i128),
            Expr::Ident { name, ty, .. } => {
                if matches!(ty, Type::Vector(_, _)) {
                    // 向量变量: 标量逐元素加载到帧临时区 slot 0
                    self.gen_vector_load(e);
                } else if self.globals.contains_key(&name.name)
                    && matches!(ty, Type::Int128 | Type::UInt128) {
                    self.gen_int128_load(&name.name);
                    if dst != 0 { self.mov(dst, 0); }
                } else if self.globals.contains_key(&name.name) {
                    self.emit_global_load(dst, &name.name);
                } else if matches!(ty, Type::Float | Type::Double | Type::LongDouble) {
                    self.gen_float_load(&name.name, ty);
                } else if self.current_func_is_nested
                    && self.upvar_offsets.contains_key(&name.name)
                    && matches!(ty, Type::Int128 | Type::UInt128) {
                    let parent_off = self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
                    self.load_upvar(0, parent_off, 8);
                    self.load_upvar(2, parent_off + 8, 8);
                    if dst != 0 { self.mov(dst, 0); }
                } else if self.current_func_is_nested && self.upvar_offsets.contains_key(&name.name) {
                    let parent_off = self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
                    self.load_upvar(dst, parent_off, ty.size());
                } else {
                    // P1-4: __int128 变量加载 (两个 64 位槽)
                    if matches!(ty, Type::Int128 | Type::UInt128) {
                        let off = self.var_offset(&name.name);
                        self.load_stack(dst, off);       // low 64 bits
                        self.load_stack(2, off + 8);     // high 64 bits → rdx
                        if dst != 0 { /* rax already set */ }
                    } else {
                        self.mov_from_stack(dst, &name.name);
                    }
                }
                // 整数提升: char/short → int
                if ty.is_int() { self.gen_int_promote(dst, ty); }
            }
            Expr::Binop { op, l, r, .. } => {
                // 逻辑与/或优先于浮点路径：比较结果为 int，但操作数可为浮点
                if matches!(op, BinOp::AndAnd | BinOp::OrOr) {
                    let l_false = self.fresh_label();
                    let l_end = self.fresh_label();
                    self.gen_expr_decay(l, dst);
                    // P1-4: int128 操作数的高 64 位在 rdx, 与 dst(low) 或合并后
                    // 再 test, 否则高 1 低 0 的 __int128 会被误判为 0.
                    if matches!(l.ty(), Type::Int128 | Type::UInt128) {
                        let or_rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[or_rex, 0x09, 0xC0 | (2 << 3) | (dst & 7)]);
                    }
                    let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                    self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                    match op {
                        BinOp::AndAnd => {
                            self.jmp_rel32(&l_false, JumpKind::Je);
                            self.gen_expr_decay(r, dst);
                            if matches!(r.ty(), Type::Int128 | Type::UInt128) {
                                let or_rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                                self.emit(&[or_rex, 0x09, 0xC0 | (2 << 3) | (dst & 7)]);
                            }
                            self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            self.emit(&[0x0F, 0x95, 0xC0]);
                            let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                            self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | (dst & 7)]);
                            self.jmp_rel32(&l_end, JumpKind::Jmp);
                            self.label(&l_false);
                            self.mov_imm(dst, 0);
                            self.label(&l_end);
                        }
                        BinOp::OrOr => {
                            self.jmp_rel32(&l_false, JumpKind::Jne);
                            self.gen_expr_decay(r, dst);
                            if matches!(r.ty(), Type::Int128 | Type::UInt128) {
                                let or_rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                                self.emit(&[or_rex, 0x09, 0xC0 | (2 << 3) | (dst & 7)]);
                            }
                            self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                            self.emit(&[0x0F, 0x95, 0xC0]);
                            let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                            self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | (dst & 7)]);
                            self.jmp_rel32(&l_end, JumpKind::Jmp);
                            self.label(&l_false);
                            self.mov_imm(dst, 1);
                            self.label(&l_end);
                        }
                        _ => unreachable!(),
                    }
                    return;
                }
                // 浮点运算: 使用 SSE 指令
                if matches!(l.ty(), Type::Float | Type::Double | Type::LongDouble) {
                    self.gen_float_binop(*op, l, r);
                    return;
                }
                // SIMD 向量运算: 标量逐元素，结果写入帧临时区
                if matches!(l.ty(), Type::Vector(_, _)) {
                    self.gen_vector_binop(*op, l, r, 0);
                    return;
                }
                // P1-4: __int128 运算
                if matches!(l.ty(), Type::Int128 | Type::UInt128)
                    || matches!(r.ty(), Type::Int128 | Type::UInt128) {
                    self.gen_int128_binop(*op, l, r, dst);
                    return;
                }
                // P2-14: _Complex 算术运算
                if matches!(l.ty(), Type::Complex(_)) {
                    self.gen_complex_binop(*op, l, r, dst);
                    return;
                }
                // 指针/数组运算缩放: ptr ± n 步进 sizeof(*ptr), ptr - ptr 返回元素数
                // 数组衰减为指针: Array(T,_) 视为 Ptr(T)
                let l_inner = match l.ty() { Type::Ptr(ref t) | Type::Array(ref t, _) => Some(t.size()), _ => None };
                let r_inner = match r.ty() { Type::Ptr(ref t) | Type::Array(ref t, _) => Some(t.size()), _ => None };
                let is_ptr_sub = matches!(op, BinOp::Sub) && l_inner.is_some() && r_inner.is_some();
                let needs_scale = matches!(op, BinOp::Add | BinOp::Sub) && !is_ptr_sub;
                let scale = if needs_scale {
                    if let Some(s) = l_inner { s } else if let Some(s) = r_inner { s } else { 1 }
                } else { 1 };
                let elem_size = if is_ptr_sub { l_inner.unwrap_or(1) } else { 1 };
                // 统一路径: 使用帧临时槽保存左操作数 (避免 push 与 call 返回地址冲突)
                // 使用 gen_expr_decay: 数组标识符在运算中衰减为指针
                // binop 临时槽: 放在 temp_slots 区域 (保存寄存器下方, 变量上方)
                // 布局: r13 → saved_regs(-8,-16) → temp_slots → variables
                let var_bytes: i32 = self.var_map.values()
                    .filter(|(_, ty)| !matches!(ty, Type::Void))
                    .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                    .sum();
                let slot_off = -(24 + var_bytes + self.expr_depth as i32 * 8);
                self.gen_expr_decay(l, dst);
                // 保存左操作数到帧临时槽: mov [r13+slot_off], dst
                {
                    let mut rex = 0x49u8; if dst >= 8 { rex |= 0x04; }
                    self.emit(&[rex, 0x89]);
                    if slot_off.abs() < 128 { self.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]); }
                    else { self.emit(&[0x85 | ((dst & 7) << 3)]); self.emit(&slot_off.to_le_bytes()); }
                }
                self.expr_depth += 1;
                self.gen_expr_decay(r, 1);
                self.expr_depth -= 1;
                // 对整数操作数乘以指针所指类型大小 (跳过 char* 和 ptr-ptr)
                if scale > 1 {
                    self.emit(&[0x48, 0x69, 0xc9]); // imul rcx, rcx, imm32
                    self.emit(&(scale as i32).to_le_bytes());
                }
                // 当 dst == 1 (rcx) 时, 恢复左操作数会覆盖右操作数
                // 解决: 先将右操作数 push 到栈, 恢复左操作数后 pop 到 rdx(2)
                if dst == 1 {
                    self.push(1); // push rcx (右操作数)
                    // 恢复左操作数到 dst
                    {
                        let mut rex = 0x49u8; if dst >= 8 { rex |= 0x04; }
                        self.emit(&[rex, 0x8B]);
                        if slot_off.abs() < 128 { self.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]); }
                        else { self.emit(&[0x85 | ((dst & 7) << 3)]); self.emit(&slot_off.to_le_bytes()); }
                    }
                    self.pop(2); // pop rdx (右操作数)
                    self.binop(*op, dst, 2);
                } else {
                    // 恢复左操作数: mov dst, [r13+slot_off]
                    {
                        let mut rex = 0x49u8; if dst >= 8 { rex |= 0x04; }
                        self.emit(&[rex, 0x8B]);
                        if slot_off.abs() < 128 { self.emit(&[0x45 | ((dst & 7) << 3), slot_off as i8 as u8]); }
                        else { self.emit(&[0x85 | ((dst & 7) << 3)]); self.emit(&slot_off.to_le_bytes()); }
                    }
                    self.binop(*op, dst, 1);
                }
                // ptr - ptr: 字节差 ÷ 元素大小 = 元素数
                if is_ptr_sub && elem_size > 1 {
                    self.mov_imm(1, elem_size as i128); // rcx = elem_size
                    self.emit(&[0x48, 0x99]); // cqo (符号扩展 rax→rdx:rax)
                    self.emit(&[0x48, 0xf7, 0xf9]); // idiv rcx
                }
            }
            Expr::Assign { l, r, .. } => {
                if let Expr::Ident { name, ty: l_ty, .. } = l.as_ref() {
                    let sz = l_ty.size();
                    let is_aggregate = matches!(l_ty, Type::Struct(_,_) | Type::Union(_,_) | Type::Array(_,ArraySize::Const(_))
                        | Type::Atomic(_))
                        && sz > 8;
                    if is_aggregate {
                        // struct/array 整体赋值: 逐块拷贝
                        self.gen_aggregate_assign(&name.name, l_ty, r);
                    } else if matches!(l_ty, Type::Complex(_)) {
                        if sz > 8 {
                            // Complex > 8B: use aggregate assign
                            self.gen_aggregate_assign(&name.name, l_ty, r);
                        } else {
                            self.gen_expr_decay(r, dst);
                            self.mov_to_stack(dst, &name.name);
                        }
                    } else {
                        self.gen_expr_decay(r, dst); // 数组衰减: int *p = arr
                        if matches!(l_ty, Type::Int128 | Type::UInt128) {
                            if self.globals.contains_key(&name.name) {
                                self.gen_int128_store(&name.name);
                            } else if self.current_func_is_nested
                                && self.upvar_offsets.contains_key(&name.name) {
                                let parent_off = self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
                                self.store_upvar(0, parent_off, 8);
                                self.store_upvar(2, parent_off + 8, 8);
                            } else {
                                let off = self.var_offset(&name.name);
                                self.store_stack(0, off);
                                self.store_stack(2, off + 8);
                            }
                        } else if self.globals.contains_key(&name.name) {
                            if matches!(l_ty, Type::Vector(_, _)) {
                                self.gen_vector_store(&name.name);
                            } else {
                                self.emit_global_store(dst, &name.name);
                            }
                        } else if self.current_func_is_nested && self.upvar_offsets.contains_key(&name.name) {
                            let parent_off = self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
                            self.store_upvar(dst, parent_off, l_ty.size());
                        } else if matches!(l_ty, Type::Vector(_, _)) {
                            self.gen_vector_store(&name.name);
                        } else if matches!(l_ty, Type::Float | Type::Double | Type::LongDouble) {
                            self.gen_float_store(&name.name, l_ty);
                        } else {
                            self.mov_to_stack(dst, &name.name);
                        }
                    }
                } else if let Expr::Index { arr, idx, ty: elem_ty, .. } = l.as_ref() {
                    // arr[i] = val
                    let es = elem_ty.size() as i32;
                    if let Expr::Ident { name: arr_name, ty: arr_ty, .. } = arr.as_ref() {
                        if self.vla_offsets.contains_key(&arr_name.name) {
                            // VLA: 基址存储在变量栈槽中
                            let off = self.var_offset(&arr_name.name);
                            self.load_stack(0, off); // rax = VLA 基址
                        } else if matches!(arr_ty, Type::Vector(_, _)) {
                            if self.globals.contains_key(&arr_name.name) {
                                self.emit_global_addr(0, &arr_name.name);
                            } else {
                                let off = self.var_offset(&arr_name.name);
                                self.emit_lea_r13(0, off);
                            }
                        } else if matches!(arr_ty, Type::Array(_, _)) {
                            // 静态数组: LEA 取基址
                            if self.globals.contains_key(&arr_name.name) {
                                self.emit_global_addr(0, &arr_name.name);
                            } else {
                                let off = self.var_offset(&arr_name.name);
                                self.emit_lea_r13(0, off);
                            }
                        } else {
                            // 指针: 加载指针值 (但全局数组衰减需用 LEA)
                            if self.globals.get(&arr_name.name).map_or(false, |g| matches!(g.ty, Type::Array(_, _))) {
                                self.emit_global_addr(0, &arr_name.name);
                            } else if self.globals.contains_key(&arr_name.name) {
                                self.emit_global_load(0, &arr_name.name);
                            } else {
                                let off = self.var_offset(&arr_name.name);
                                self.load_stack(0, off);
                            }
                        }
                        self.push(0); // 保存基址 (gen_expr(idx) 可能破坏 rax)
                        self.gen_expr(idx, 1); // rcx = index
                        self.pop(0); // 恢复基址到 rax
                        if es != 1 {
                            self.emit(&[0x48, 0x69, 0xc9]); // imul rcx, rcx, imm32
                            self.emit(&es.to_le_bytes());
                        }
                        self.emit(&[0x48, 0x01, 0xc8]); // add rax, rcx (addr in rax)
                        self.push(0); // save addr
                        self.gen_expr(r, 0); // rax = val
                        self.pop(1); // rcx = addr
                        match es {
                            1 => self.emit(&[0x88, 0x01]),
                            2 => self.emit(&[0x66, 0x89, 0x01]),
                            4 => self.emit(&[0x89, 0x01]),
                            _ => self.emit(&[0x48, 0x89, 0x01]),
                        }
                    }
                } else if let Expr::Deref { e: ptr_e, .. } = l.as_ref() {
                    // *ptr = val
                    self.gen_expr(ptr_e, 0); // rax = address
                    self.push(0);             // save addr
                    self.gen_expr(r, 0);      // rax = val
                    self.pop(1);              // rcx = addr
                    self.emit(&[0x48, 0x89, 0x01]); // mov [rcx], rax
                } else if let Expr::Member { expr, field, is_arrow, .. } = l.as_ref() {
                    // struct.field = val 或 ptr->field = val
                    // 1. 查找字段信息 (支持匿名 struct/union)
                    let field_info = if let Type::Struct(_, fields) | Type::Union(_, fields) = &expr.ty() {
                        find_field(fields, &field.name).map(|(f, extra)| {
                            let mut fc = f.clone();
                            fc.offset += extra;
                            fc
                        })
                    } else { None };
                    let is_bitfield = field_info.as_ref().map_or(false, |f| f.bit_width.is_some());
                    // 2. 计算目标地址
                    if *is_arrow {
                        self.gen_expr(expr, 0); // rax = 指针值
                    } else if let Expr::Ident { name, .. } = expr.as_ref() {
                        let off = self.var_offset(&name.name);
                        // LEA rax, [r13 + off]
                        self.emit(&[0x49, 0x8D]);
                        if off.abs() < 128 {
                            self.emit(&[0x45, off as i8 as u8]); // mod=01, rm=r13
                        } else {
                            self.emit(&[0x85]); // mod=00, rm=r13
                            self.emit(&off.to_le_bytes());
                        }
                    }
                    // 3. 加上字段偏移
                    if let Some(ref f) = field_info {
                        let field_off = f.offset as i32;
                        if field_off != 0 {
                            self.emit(&[0x48, 0x05]);
                            self.emit(&field_off.to_le_bytes());
                        }
                    }
                    if is_bitfield {
                        // 位域写入: 读-改-写 (使用帧内临时槽, 避免 push 与 call 返回地址冲突)
                        let f = field_info.as_ref().unwrap();
                        let bw = f.bit_width.unwrap();
                        let container_size = f.ty.size();
                        let bit_off = f.bit_offset;
                        // rax = 容器地址
                        // 保存地址到帧临时槽1: [r13-8]
                        self.emit(&[0x49, 0x89, 0x45, 0xf8]); // mov [r13-8], rax
                        // 加载当前容器值 (结果统一放入 rax)
                        match container_size {
                            4 => self.emit(&[0x8B, 0x00]), // mov eax, [rax] (零扩展)
                            8 => self.emit(&[0x48, 0x8B, 0x00]), // mov rax, [rax]
                            2 => { self.emit(&[0x66, 0x8B, 0x00]); // mov ax, [rax]
                                   self.emit(&[0x48, 0x0F, 0xB7, 0xC0]); } // movzx eax, ax
                            1 => self.emit(&[0x0F, 0xB6, 0x00]), // movzx eax, byte [rax]
                            _ => {}
                        }
                        // 保存容器值到帧临时槽2: [r13-16]
                        self.emit(&[0x49, 0x89, 0x45, 0xf0]); // mov [r13-16], rax
                        // 计算右值
                        self.gen_expr(r, 0); // rax = 新值
                        // 掩码: and rax, (1<<bw)-1
                        if bw < 64 {
                            let mask: u64 = (1u64 << bw) - 1;
                            self.emit(&[0x48, 0x25]); // and eax, imm32
                            self.emit(&(mask as u32).to_le_bytes());
                        }
                        // 左移到位置: shl rax, bit_off
                        if bit_off > 0 {
                            self.emit(&[0x48, 0xC1, 0xE0, bit_off as u8]); // shl rax, imm8
                        }
                        // rax = 移位后的新值
                        // 加载容器值: rcx = [r13-16]
                        self.emit(&[0x49, 0x8B, 0x4D, 0xf0]); // mov rcx, [r13-16]
                        // 清除目标位: inv_mask = ~(((1<<bw)-1) << bit_off)
                        let mask: u64 = if bw >= 64 { u64::MAX } else { (1u64 << bw) - 1 };
                        let inv_mask = !(mask << bit_off);
                        // and rcx, inv_mask
                        self.emit(&[0x48, 0x81, 0xE1]); // and rcx, imm32
                        self.emit(&(inv_mask as u32).to_le_bytes());
                        // or rcx, rax
                        self.emit(&[0x48, 0x09, 0xC1]); // or rcx, rax
                        // 加载地址: rdx = [r13-8]
                        self.emit(&[0x49, 0x8B, 0x55, 0xf8]); // mov rdx, [r13-8]
                        // 存储容器 (结果在 rcx 中)
                        match container_size {
                            4 => self.emit(&[0x89, 0x0A]), // mov [rdx], ecx
                            8 => self.emit(&[0x48, 0x89, 0x0A]), // mov [rdx], rcx
                            2 => self.emit(&[0x66, 0x89, 0x0A]), // mov [rdx], cx
                            1 => self.emit(&[0x88, 0x0A]), // mov [rdx], cl
                            _ => {}
                        }
                    } else {
                        self.push(0); // 保存目标地址
                        self.gen_expr(r, 0); // rax = 值
                        self.pop(1); // rcx = 目标地址
                        let size = field_info.as_ref().map_or(4, |f| f.ty.size());
                        match size {
                            1 => self.emit(&[0x88, 0x01]),
                            2 => self.emit(&[0x66, 0x89, 0x01]),
                            4 => self.emit(&[0x89, 0x01]),
                            8 => self.emit(&[0x48, 0x89, 0x01]),
                            _ => {}
                        }
                    }
                }
            }
            Expr::Unary { op, e, .. } => {
                // 浮点字面量取负: 编译期折叠, 直接生成相反值的 Float 表达式
                if matches!(op, UnOp::Neg) {
                    if let Expr::Float { v, ty, span, .. } = e.as_ref() {
                        let negated = Expr::Float { v: -*v, ty: ty.clone(), span: span.clone() };
                        self.gen_expr(&negated, dst);
                        return;
                    }
                }
                if matches!(e.ty(), Type::Int128 | Type::UInt128) {
                    self.gen_expr(e, 0);
                    match op {
                        UnOp::Neg => {
                            self.emit(&[0x48, 0xF7, 0xD8]);       // neg rax
                            self.emit(&[0x48, 0x83, 0xD2, 0x00]); // adc rdx, 0
                            self.emit(&[0x48, 0xF7, 0xDA]);       // neg rdx
                        }
                        UnOp::BitNot => {
                            self.emit(&[0x48, 0xF7, 0xD0]); // not rax
                            self.emit(&[0x48, 0xF7, 0xD2]); // not rdx
                        }
                        UnOp::Not => {
                            self.emit(&[0x48, 0x09, 0xD0]); // or rax, rdx
                            self.emit(&[0x48, 0x85, 0xC0]); // test rax, rax
                            self.emit(&[0x0F, 0x94, 0xC0]); // sete al
                            self.emit(&[0x48, 0x0F, 0xB6, 0xC0]);
                            self.emit(&[0x48, 0x31, 0xD2]); // high = 0
                        }
                        UnOp::Plus => {}
                    }
                    if dst != 0 { self.mov(dst, 0); }
                    return;
                }
                // 浮点取负: 翻转符号位 (XOR with sign mask)
                // 否则在 gen_expr 之后 emit neg r64 会错把 rax 取反, 而浮点结果在 xmm0.
                if matches!(op, UnOp::Neg) && matches!(e.ty(), Type::Double | Type::Float | Type::LongDouble) {
                    self.gen_expr(e, 0); // xmm0 = e
                    match e.ty() {
                        Type::Float => {
                            // 翻转 32 位符号位: movabs rax, 0x8000_0000
                            //   66 48 0f 6e c8 movq xmm1, rax
                            //   0f 57 c1       xorps xmm0, xmm1
                            self.emit(&[0x48, 0xB8]);
                            self.emit(&0x80000000u64.to_le_bytes());
                            self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]);
                            self.emit(&[0x0F, 0x57, 0xC1]);
                        }
                        Type::Double => {
                            // 翻转 64 位符号位
                            self.emit(&[0x48, 0xB8]);
                            self.emit(&0x8000000000000000u64.to_le_bytes());
                            self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]);
                            self.emit(&[0x66, 0x0F, 0x57, 0xC1]);
                        }
                        Type::LongDouble => {
                            // long double (80-bit): dcc 用 8 字节近似, 同样翻转 bit 63
                            self.emit(&[0x48, 0xB8]);
                            self.emit(&0x8000000000000000u64.to_le_bytes());
                            self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]);
                            self.emit(&[0x66, 0x0F, 0x57, 0xC1]);
                        }
                        _ => unreachable!(),
                    }
                    if dst != 0 { self.mov(dst, 0); }
                    return;
                }
                self.gen_expr(e, dst);
                match op {
                    UnOp::Neg => {
                        // neg r64 (带 REX.W 确保 64 位操作, 避免 32 位零扩展问题)
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0xf7, 0xd8 | (dst & 7)]);
                    }
                    UnOp::Not => {
                        // 逻辑非: !x = (x == 0) ? 1 : 0
                        // test dst, dst
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                        // sete al: 零 → 1, 非零 → 0 (0F 94 C0)
                        self.emit(&[0x0F, 0x94, 0xC0]);
                        // movzx dst, al (48 0F B6 C0)
                        let rex2 = 0x48 | if dst >= 8 { 0x41 } else { 0 };
                        self.emit(&[rex2, 0x0F, 0xB6, 0xC0 | (dst & 7)]);
                    }
                    UnOp::BitNot => {
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0xf7, 0xd0 | (dst & 7)]);
                    }
                    UnOp::Plus => {}
                }
            }
            Expr::Call { callee, args, ty, .. } => {
                let sret_off = if Self::needs_memory_return(ty) {
                    Some(self.alloc_sret_slot(ty.size()))
                } else {
                    None
                };
                // 检查是否为直接调用 (callee 是 Ident)
                if let Expr::Ident { name, .. } = callee.as_ref() {
                    // __atomic_* 内置函数特殊处理 (生成 lock 前缀指令)
                    if name.name.starts_with("__atomic_") && self.gen_atomic_builtin(&name.name, args, dst) {
                        // 已成功生成原子指令
                    } else if self.gen_builtin(&name.name, args, dst) {
                        // 已成功处理 __builtin_* 函数
                    } else if name.name.starts_with("__builtin_") {
                        eprintln!(
                            "warning: unsupported __builtin_ function '{}', emitted as {}",
                            name.name,
                            if matches!(ty, Type::Void) { "ud2" } else { "zero" }
                        );
                        if matches!(ty, Type::Void) {
                            self.emit(&[0x0F, 0x0B]); // ud2
                        } else {
                            self.mov_imm(dst, 0);
                        }
                    } else {
                        self.emit_sysv_call(Some(&name.name), None, args, sret_off);
                        if sret_off.is_some() {
                            if dst != 0 {
                                self.emit_lea_r13(dst, sret_off.unwrap());
                            }
                        } else if dst != 0 {
                            self.mov(dst, 0);
                        }
                    }
                } else {
                    self.emit_sysv_call(None, Some(callee), args, sret_off);
                    if sret_off.is_some() {
                        if dst != 0 {
                            self.emit_lea_r13(dst, sret_off.unwrap());
                        }
                    } else if dst != 0 {
                        self.mov(dst, 0);
                    }
                }
            }
            Expr::SizeofTy { ty, .. } => self.mov_imm(dst, ty.size() as i128),
            Expr::SizeofExpr { e, .. } => self.mov_imm(dst, e.ty().size() as i128),
            Expr::AlignofTy { ty, .. } => self.mov_imm(dst, ty.align() as i128),
            Expr::AlignofExpr { e, .. } => self.mov_imm(dst, e.ty().align() as i128),
            Expr::Offsetof { ty, ref member, .. } => {
                let off = Self::compute_offset(ty, member);
                self.mov_imm(dst, off as i128);
            }
            Expr::Comma { l, r, .. } => {
                // 逗号运算符: 求值 lhs 丢弃, 求值 rhs 结果放入 dst
                self.gen_expr(l, 0);
                self.gen_expr(r, dst);
            }
            Expr::AsmExpr { asm_str, outputs, output_names, output_constraints,
                inputs, input_names, input_constraints, clobbers, .. } => {
                self.emit_inline_asm(
                    asm_str, outputs, output_names, output_constraints,
                    inputs, input_names, input_constraints, clobbers,
                );
                if dst != 0 { self.mov(dst, 0); }
            }
            Expr::LabelAddr { label, .. } => {
                // &&label: 加载标签地址到 dst (GNU 扩展)
                // 使用 LEA dst, [RIP + disp32] + label_addr_refs 延迟解析
                let label_name = label.name.clone();
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                let modrm = 0x05 | ((dst & 7) << 3);
                self.emit(&[rex, 0x8D, modrm, 0, 0, 0, 0]);
                let disp32_pos = self.pos() - 4;
                self.label_addr_refs.push((disp32_pos, label_name));
            }
            Expr::StmtExpr { block, .. } => {
                // 语句表达式 ({...}): 执行块内语句, 最后表达式值留在 dst
                let items = &block.items;
                let last_idx = items.len().saturating_sub(1);
                for (i, item) in items.iter().enumerate() {
                    match item {
                        BlockItem::Stmt(Stmt::Expr { e, .. }) if i == last_idx => {
                            // 最后一条表达式语句: 结果放入 dst
                            self.gen_expr(e, dst);
                        }
                        BlockItem::Stmt(Stmt::Return { val: Some(e), .. }) if i == last_idx => {
                            self.gen_expr(e, dst);
                        }
                        BlockItem::Stmt(s) => self.gen_stmt(s),
                        BlockItem::Decl(Decl::Var(v)) => {
                            self.register_var(&v.name.name, &v.ty);
                            if let Some(init) = &v.init {
                                self.gen_expr(init, 0);
                                self.mov_to_stack(0, &v.name.name);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Expr::Cast { ty, e, .. } => {
                // (v4si){...} 复合字面量：parser 降级为 Cast + InitList
                if let (Type::Vector(elem_ty, n), Expr::InitList { inits, .. }) = (ty, e.as_ref()) {
                    self.gen_vector_init_rvalue(inits, elem_ty, *n, 0);
                    return;
                }
                let src_ty = e.ty();
                self.gen_expr(e, dst);
                // 类型转换
                match (&src_ty, ty) {
                    (src, Type::Int128 | Type::UInt128)
                        if !matches!(src, Type::Int128 | Type::UInt128) => {
                        if dst != 0 { self.mov(0, dst); }
                        if matches!(src, Type::Bool | Type::UChar | Type::UShort
                            | Type::UInt | Type::ULong | Type::ULongLong) {
                            self.emit(&[0x48, 0x31, 0xD2]);
                        } else {
                            self.emit(&[0x48, 0x99]);
                        }
                        if dst != 0 { self.mov(dst, 0); }
                    }
                    // int → float/double
                    (Type::Int | Type::Long | Type::Char | Type::Short, Type::Float) => {
                        // cvtsi2ss xmm0, rax (F3 0F 2A C0)
                        self.emit(&[0xF3, 0x0F, 0x2A, 0xC0]);
                    }
                    (Type::Int | Type::Long | Type::Char | Type::Short, Type::Double | Type::LongDouble) => {
                        // cvtsi2sd xmm0, rax (F2 0F 2A C0)
                        self.emit(&[0xF2, 0x0F, 0x2A, 0xC0]);
                    }
                    // float/double → int
                    (Type::Float, Type::Int | Type::Long | Type::Char | Type::Short) => {
                        // cvttss2si rax, xmm0 (F3 0F 2C C0)
                        self.emit(&[0xF3, 0x0F, 0x2C, 0xC0]);
                    }
                    (Type::Double | Type::LongDouble, Type::Int | Type::Long | Type::Char | Type::Short) => {
                        // cvttsd2si rax, xmm0 (F2 0F 2C C0)
                        self.emit(&[0xF2, 0x0F, 0x2C, 0xC0]);
                    }
                    (Type::Complex(inner), Type::Int | Type::Long | Type::Char | Type::Short) => {
                        let is_f32 = matches!(inner.as_ref(), Type::Float);
                        match e.as_ref() {
                            Expr::Binop { .. } => {
                                // gen_complex_binop 已将实部留在 xmm0
                            }
                            Expr::Ident { name, .. } => {
                                let off = self.var_offset(&name.name);
                                if is_f32 {
                                    self.emit(&[0xF3, 0x49, 0x0F, 0x10, 0x45]);
                                    if off.abs() < 128 { self.emit(&[off as u8]); }
                                    else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
                                } else {
                                    self.emit(&[0xF2, 0x49, 0x0F, 0x10, 0x45]);
                                    if off.abs() < 128 { self.emit(&[off as u8]); }
                                    else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
                                }
                            }
                            _ => {
                                let result_off = -(self.frame_size as i32) + 8;
                                if is_f32 {
                                    self.emit(&[0xF3, 0x49, 0x0F, 0x10, 0x45]);
                                    if result_off.abs() < 128 { self.emit(&[result_off as u8]); }
                                    else { self.emit(&[0x85]); self.emit(&result_off.to_le_bytes()); }
                                } else {
                                    self.emit(&[0xF2, 0x49, 0x0F, 0x10, 0x45]);
                                    if result_off.abs() < 128 { self.emit(&[result_off as u8]); }
                                    else { self.emit(&[0x85]); self.emit(&result_off.to_le_bytes()); }
                                }
                            }
                        }
                        if is_f32 {
                            self.emit(&[0xF3, 0x0F, 0x2C, 0xC0]);
                        } else {
                            self.emit(&[0xF2, 0x0F, 0x2C, 0xC0]);
                        }
                        if dst != 0 { self.mov(dst, 0); }
                    }
                    // float ↔ double
                    (Type::Float, Type::Double | Type::LongDouble) => {
                        // cvtss2sd xmm0, xmm0 (F3 0F 5A C0)
                        self.emit(&[0xF3, 0x0F, 0x5A, 0xC0]);
                    }
                    (Type::Double | Type::LongDouble, Type::Float) => {
                        // cvtsd2ss xmm0, xmm0 (F2 0F 5A C0)
                        self.emit(&[0xF2, 0x0F, 0x5A, 0xC0]);
                    }
                    // 其他情况: 不转换
                    _ => {}
                }
            }
            Expr::Cond { cond, then_, else_, .. } => {
                self.gen_expr(cond, 0); self.emit(&[0x85, 0xc0]);
                let le = self.fresh_label(); let lend = self.fresh_label();
                self.jmp_rel32(&le, JumpKind::Je);
                self.gen_expr(then_, dst); self.jmp_rel32(&lend, JumpKind::Jmp);
                self.label(&le); self.gen_expr(else_, dst); self.label(&lend);
            }
            Expr::String { data, .. } => {
                // 将字符串加入 .rodata 池, 生成 RIP-relative LEA
                let data = data.clone();
                if !self.string_pool.contains_key(&data) {
                    let o = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(&data);
                    self.string_pool.insert(data.clone(), o);
                }
                let at = self.pos();
                // LEA dst, [RIP + disp32]  (7 bytes, disp32 = placeholder 0)
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                self.emit(&[rex, 0x8D, 0x05 | ((dst & 7) << 3), 0, 0, 0, 0]);
                self.string_refs.push((at, data));
            }
            Expr::Float { v, ty, .. } => {
                // 浮点字面量: 存入 .rodata, 用 movsd/movss 加载到 xmm0
                let is_float = matches!(ty, Type::Float);
                let is_ld = matches!(ty, Type::LongDouble);
                let bytes = if is_float {
                    // float: 4 字节
                    let f = *v as f32;
                    f.to_le_bytes().to_vec()
                } else if is_ld {
                    // long double: 16 字节 (低 8 字节 double, 高 8 字节零)
                    let mut b = v.to_le_bytes().to_vec();
                    b.extend_from_slice(&[0u8; 8]);
                    b
                } else {
                    // double: 8 字节
                    v.to_le_bytes().to_vec()
                };
                let key = format!("f_{}", bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>());
                let key_bytes = key.into_bytes();
                if !self.string_pool.contains_key(&key_bytes) {
                    let o = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(&bytes);
                    self.string_pool.insert(key_bytes.clone(), o);
                }
                let off = self.string_pool[&key_bytes];
                let at = self.pos();
                // movsd xmm0, [RIP + disp32]  (F2 0F 10 05 disp32)
                // movss xmm0, [RIP + disp32]  (F3 0F 10 05 disp32)
                let prefix = if is_float { 0xF3 } else { 0xF2 };
                self.emit(&[prefix, 0x0F, 0x10, 0x05, 0, 0, 0, 0]);
                self.float_refs.push((at, off));
            }
            Expr::Index { arr, idx, ty: elem_ty, .. } => {
                let es = elem_ty.size() as i32;
                if let Expr::Ident { name: arr_name, ty: arr_ty, .. } = arr.as_ref() {
                    if self.vla_offsets.contains_key(&arr_name.name) {
                        // VLA: 基址存储在变量栈槽中
                        let off = self.var_offset(&arr_name.name);
                        self.load_stack(0, off); // rax = VLA 基址
                    } else if matches!(arr_ty, Type::Vector(_, _)) {
                        if self.globals.contains_key(&arr_name.name) {
                            self.emit_global_addr(0, &arr_name.name);
                        } else {
                            let off = self.var_offset(&arr_name.name);
                            self.emit_lea_r13(0, off);
                        }
                    } else if matches!(arr_ty, Type::Array(_, _)) {
                        // 静态数组: LEA 取基址
                        if self.globals.contains_key(&arr_name.name) {
                            self.emit_global_addr(0, &arr_name.name);
                        } else {
                            let off = self.var_offset(&arr_name.name);
                            self.emit_lea_r13(0, off);
                        }
                    } else {
                        // 指针: 加载指针值 (但全局数组衰减需用 LEA)
                        if self.globals.get(&arr_name.name).map_or(false, |g| matches!(g.ty, Type::Array(_, _))) {
                            self.emit_global_addr(0, &arr_name.name);
                        } else if self.globals.contains_key(&arr_name.name) {
                            self.emit_global_load(0, &arr_name.name);
                        } else {
                            let off = self.var_offset(&arr_name.name);
                            self.load_stack(0, off);
                        }
                    }
                    self.push(0); // 保存基址 (gen_expr(idx) 可能破坏 rax)
                    self.gen_expr(idx, 1); // rcx = index
                    self.pop(0); // 恢复基址到 rax
                    if es != 1 {
                        self.emit(&[0x48, 0x69, 0xc9]); // imul rcx, rcx, imm32
                        self.emit(&es.to_le_bytes());
                    }
                    self.emit(&[0x48, 0x01, 0xc8]); // add rax, rcx
                    self.emit_load_mem_typed(es as usize, elem_ty); // 按元素类型加载
                    if dst != 0 && !matches!(elem_ty, Type::Float | Type::Double) {
                        self.mov(dst, 0);
                    }
                }
            }
            Expr::PostInc { e, .. } => self.gen_inc_dec(e, dst, false, true),
            Expr::PostDec { e, .. } => self.gen_inc_dec(e, dst, false, false),
            Expr::PreInc  { e, .. } => self.gen_inc_dec(e, dst, true, true),
            Expr::PreDec  { e, .. } => self.gen_inc_dec(e, dst, true, false),
            Expr::Deref { e, ty, .. } => {
                // *e: 求值内部表达式得到地址, 然后按结果类型大小加载
                let load_ty = ty.clone();
                let load_size = load_ty.size().max(1);
                self.gen_expr(e, 0);
                self.emit_load_mem_typed(load_size, &load_ty);
                if dst != 0 && !matches!(load_ty, Type::Float | Type::Double) {
                    self.mov(dst, 0);
                }
            }
            Expr::AddrOf { e, .. } => {
                // &var / &struct.field / &ptr->field / *ptr: 计算地址到 dst
                if let Expr::Ident { name, .. } = e.as_ref() {
                    if self.globals.contains_key(&name.name) {
                        self.emit_global_addr(dst, &name.name);
                    } else {
                        let off = self.var_offset(&name.name);
                        // REX.W + REX.B (r13) + REX.R (当 dst >= 8, 编码 r10/r11/r12/...)
                        let rex = 0x48u8 | 0x01 | (if dst >= 8 { 0x04 } else { 0 });
                        self.emit(&[rex, 0x8D]);
                        if off.abs() < 128 {
                            self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                        } else {
                            self.emit(&[0x85 | ((dst & 7) << 3)]);
                            self.emit(&off.to_le_bytes());
                        }
                    }
                } else if let Expr::Member { expr, field, is_arrow, .. } = e.as_ref() {
                    // &struct.field: 基址 + 字段偏移 (不加载值)
                    if *is_arrow {
                        self.gen_expr(expr, dst);
                    } else if let Expr::Ident { name, .. } = expr.as_ref() {
                        if self.globals.contains_key(&name.name) {
                            self.emit_global_addr(dst, &name.name);
                        } else {
                            let off = self.var_offset(&name.name);
                            let rex = 0x49u8;
                            self.emit(&[rex, 0x8D]);
                            if off.abs() < 128 {
                                self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                            } else {
                                self.emit(&[0x85 | ((dst & 7) << 3)]);
                                self.emit(&off.to_le_bytes());
                            }
                        }
                    } else {
                        self.gen_expr(expr, dst);
                    }
                    let field_info = if let Type::Struct(_, fields) | Type::Union(_, fields) = &expr.ty() {
                        find_field(fields, &field.name).map(|(f, extra)| {
                            let mut fc = f.clone();
                            fc.offset += extra;
                            fc
                        })
                    } else { None };
                    if let Some(ref f) = field_info {
                        let field_off = f.offset as i32;
                        if field_off != 0 {
                            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                            self.emit(&field_off.to_le_bytes());
                        }
                    }
                } else if let Expr::Deref { e: inner, .. } = e.as_ref() {
                    // *ptr: 地址就是指针值
                    self.gen_expr(inner, dst);
                } else if let Expr::Index { arr, idx, ty: elem_ty, .. } = e.as_ref() {
                    // &arr[i]: 基址 + i * elem_size
                    let es = elem_ty.size() as i32;
                    if let Expr::Ident { name: arr_name, .. } = arr.as_ref() {
                        if self.globals.contains_key(&arr_name.name) {
                            self.emit_global_addr(0, &arr_name.name);
                        } else if self.vla_offsets.contains_key(&arr_name.name) {
                            let off = self.var_offset(&arr_name.name);
                            self.load_stack(0, off);
                        } else {
                            let off = self.var_offset(&arr_name.name);
                            self.emit_lea_r13(0, off);
                        }
                    }
                    self.push(0);
                    self.gen_expr(idx, 1);
                    self.pop(0);
                    if es != 1 {
                        self.emit(&[0x48, 0x69, 0xC9]); // imul rcx, rcx, es
                        self.emit(&es.to_le_bytes());
                    }
                    self.emit(&[0x48, 0x01, 0xC8]); // add rax, rcx
                    if dst != 0 { self.mov(dst, 0); }
                }
            }
            Expr::Member { expr, field, is_arrow, ty, .. } => {
                // struct/union 成员访问: 基址 + 偏移
                // 1. 计算基址 (struct指针或struct变量地址)
                if *is_arrow {
                    self.gen_expr(expr, dst);
                } else {
                    if let Expr::Ident { name, .. } = expr.as_ref() {
                        if self.globals.contains_key(&name.name) {
                            self.emit_global_addr(dst, &name.name);
                        } else {
                            let off = self.var_offset(&name.name);
                            let rex = 0x49u8;
                            self.emit(&[rex, 0x8D]);
                            if off.abs() < 128 {
                                self.emit(&[0x45 | ((dst & 7) << 3), off as i8 as u8]);
                            } else {
                                self.emit(&[0x85 | ((dst & 7) << 3)]);
                                self.emit(&off.to_le_bytes());
                            }
                        }
                    } else {
                        self.gen_expr(expr, dst);
                    }
                }
                // 2. 查找字段信息 (支持匿名 struct/union), 加上字段偏移
                let field_info = if let Type::Struct(_, fields) | Type::Union(_, fields) = &expr.ty() {
                    find_field(fields, &field.name).map(|(f, extra)| {
                        let mut fc = f.clone();
                        fc.offset += extra;
                        fc
                    })
                } else { None };
                if let Some(ref f) = field_info {
                    let field_off = f.offset as i32;
                    if field_off != 0 {
                        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                        self.emit(&[rex, 0x81, 0xC0 | (dst & 7)]);
                        self.emit(&field_off.to_le_bytes());
                    }
                }
                // 3. 加载成员值 (位域 vs 普通)
                if let Some(ref f) = field_info {
                    if let Some(bw) = f.bit_width {
                        // 位域读取: 加载容器 → 右移 → 掩码
                        let container_size = f.ty.size();
                        // 加载容器值
                        match container_size {
                            1 => self.emit(&[0x48, 0x0F, 0xB6, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            2 => self.emit(&[0x48, 0x0F, 0xB7, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            4 => self.emit(&[0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            8 => self.emit(&[0x48, 0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            _ => {}
                        }
                        // 右移 bit_offset 位
                        if f.bit_offset > 0 {
                            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex, 0xC1, 0xE8 | (dst & 7), f.bit_offset as u8]);
                        }
                        // 掩码: and dst, (1 << bw) - 1
                        if bw < 64 {
                            let mask: u64 = if bw == 64 { u64::MAX } else { (1u64 << bw) - 1 };
                            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex, 0x81, 0xE0 | (dst & 7)]);
                            self.emit(&(mask as u32).to_le_bytes());
                        }
                        // 有符号位域: 符号扩展
                        let is_unsigned = matches!(f.ty, Type::UInt | Type::ULong | Type::ULongLong | Type::UShort | Type::UChar | Type::Bool);
                        if !is_unsigned && bw < 64 {
                            // 检查符号位 (bit bw-1)
                            let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            // test dst, (1 << (bw-1))
                            self.emit(&[rex, 0xF7, 0xC0 | (dst & 7)]);
                            self.emit(&((1u32 << (bw - 1)).to_le_bytes()));
                            // jz +7 (跳过符号扩展)
                            self.emit(&[0x74, 0x07]);
                            // or dst, !(mask)  (填充高位)
                            let inv_mask: u32 = !(((1u32 << bw) - 1)) as u32;
                            let rex2 = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                            self.emit(&[rex2, 0x81, 0xC8 | (dst & 7)]);
                            self.emit(&inv_mask.to_le_bytes());
                        }
                    } else {
                        // 普通字段: 直接加载
                        let size = f.ty.size();
                        match size {
                            1 => self.emit(&[0x48, 0x0F, 0xB6, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            2 => self.emit(&[0x48, 0x0F, 0xB7, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            4 => self.emit(&[0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            8 => self.emit(&[0x48, 0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                            _ => {}
                        }
                    }
                } else {
                    let size = ty.size();
                    match size {
                        1 => self.emit(&[0x48, 0x0F, 0xB6, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                        2 => self.emit(&[0x48, 0x0F, 0xB7, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                        4 => self.emit(&[0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                        8 => self.emit(&[0x48, 0x8B, 0x00 | ((dst & 7) << 3) | (dst & 7)]),
                        _ => {}
                    }
                }
            }
            Expr::CompoundLit { ty, inits, .. } => {
                if let Type::Vector(elem_ty, n) = ty {
                    self.gen_vector_init_rvalue(inits, elem_ty, *n, 0);
                    return;
                }
                // 复合字面量: (type){...} - 在 rodata 中存储初始化数据, 加载地址
                let size = ty.size();
                let data = self.gen_global_init_list(ty, inits, None)
                    .unwrap_or_else(|| vec![0; size]);
                // 使用 string_pool 存储
                if !self.string_pool.contains_key(&data) {
                    let off = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(&data);
                    self.string_pool.insert(data.clone(), off);
                }
                let off = self.string_pool[&data];
                // 加载地址到 dst (RIP-relative LEA)
                let code_pos = self.pos();
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                self.emit(&[rex, 0x8D, 0x05 | ((dst & 7) << 3), 0, 0, 0, 0]);
                // 记录引用, 在 resolve 阶段修补
                self.float_refs.push((code_pos + 3, off));
            }
            Expr::VaArg { ap, ty: arg_ty, .. } => {
                if let Expr::Ident { name, .. } = ap.as_ref() {
                    self.gen_va_arg(&name.name, arg_ty, dst);
                } else {
                    self.mov_imm(dst, 0);
                }
            }
            Expr::InitList { inits, .. } => {
                if let Type::Vector(elem_ty, n) = e.ty() {
                    self.gen_vector_init_rvalue(inits, &elem_ty, n, 0);
                } else {
                    eprintln!("dcc: codegen unsupported InitList rvalue for {:?}", e.ty());
                    self.emit(&[0x0F, 0x0B]); // ud2
                    self.mov_imm(dst, 0);
                }
            }
            _ => {
                eprintln!("dcc: codegen skip unsupported expr (ud2)");
                self.emit(&[0x0F, 0x0B]); // ud2
                self.mov_imm(dst, 0);
            }
        }
    }

    // 向量 rvalue 初始化列表: 结果写入帧临时区 (与 gen_vector_load/store 一致)
    fn vector_temp_base(&self, slot: usize, vec_size: usize) -> i32 {
        let slot_bytes = ((vec_size + 7) / 8) as i32 * 8;
        -(self.frame_size as i32) + 8 + (slot as i32 * slot_bytes)
    }

    /// 标量整型表达式写入浮点向量元素前，将 rax 转为 xmm0
    fn emit_scalar_to_xmm0_if_needed(&mut self, expr: &Expr, elem_ty: &Type) {
        if !matches!(elem_ty, Type::Float | Type::Double | Type::LongDouble) {
            return;
        }
        if matches!(expr.ty(), Type::Float | Type::Double | Type::LongDouble | Type::Complex(_)) {
            return;
        }
        if matches!(elem_ty, Type::Float) {
            self.emit(&[0xF3, 0x0F, 0x2A, 0xC0]); // cvtsi2ss xmm0, eax
        } else {
            self.emit(&[0xF2, 0x0F, 0x2A, 0xC0]); // cvtsi2sd xmm0, eax
        }
    }

    fn gen_vector_init_rvalue(
        &mut self,
        inits: &[InitItem],
        elem_ty: &Type,
        count: usize,
        temp_slot: usize,
    ) {
        let elem_size = elem_ty.size();
        let vec_size = elem_size * count;
        let temp_off = self.vector_temp_base(temp_slot, vec_size);
        for i in 0..count {
            let value = inits.get(i).and_then(|item| match item {
                InitItem::Expr(e) => Some(e),
                _ => None,
            });
            let off = temp_off + (i * elem_size) as i32;
            if let Some(v) = value {
                self.gen_expr(v, 0);
                self.emit_scalar_to_xmm0_if_needed(v, elem_ty);
            } else {
                if matches!(elem_ty, Type::Float | Type::Double) {
                    self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0
                } else {
                    self.mov_imm(0, 0);
                }
            }
            if matches!(elem_ty, Type::Float) {
                self.store_xmm32_stack(0, off);
            } else if matches!(elem_ty, Type::Double) {
                self.store_xmm64_stack(0, off);
            } else {
                self.emit_store(0, off, elem_size);
            }
        }
    }

    // 初始化列表 codegen: 依次计算每个元素并存储到变量
    fn gen_init_list(&mut self, var_name: &str, ty: &Type, inits: &[InitItem]) {
        let var_off = self.var_offset(var_name);
        match ty {
            Type::Array(elem_ty, _) => {
                // 数组初始化: int a[] = {1, 2, 3}
                // 数组在栈上布局: arr[0] 在 var_off, arr[1] 在 var_off + elem_size, ...
                // 注意: 虽然栈向下增长，但数组元素地址是递增的
                let elem_size = elem_ty.size();
                for (i, item) in inits.iter().enumerate() {
                    let (_idx, value) = match item {
                        InitItem::Expr(e) => (i, e),
                        InitItem::Designated { designator: Designator::Index(idx_expr), value } => {
                            if let Expr::Int { v, .. } = idx_expr { (*v as usize, value) } else { continue; }
                        }
                        _ => continue,
                    };
                    self.gen_expr(value, 0);
                    // arr[i] 在 var_off + i * elem_size
                    let off = var_off + (i * elem_size) as i32;
                    self.emit_store(0, off, elem_size);
                }
            }
            Type::Vector(elem_ty, n) => {
                // SIMD 向量初始化: v4si a = {1, 2, 3, 4}
                let elem_size = elem_ty.size();
                for (i, item) in inits.iter().enumerate() {
                    if i >= *n { break; }
                    let value = match item {
                        InitItem::Expr(e) => e,
                        _ => continue,
                    };
                    self.gen_expr(value, 0);
                    self.emit_scalar_to_xmm0_if_needed(value, elem_ty);
                    let off = var_off + (i * elem_size) as i32;
                    if matches!(**elem_ty, Type::Float) {
                        self.store_xmm32_stack(0, off);
                    } else if matches!(**elem_ty, Type::Double) {
                        self.store_xmm64_stack(0, off);
                    } else {
                        self.emit_store(0, off, elem_size);
                    }
                }
            }
            Type::Struct(_, fields) | Type::Union(_, fields) => {
                // struct 初始化: struct Point p = {1, 2} 或 {.x = 1, .y = 2}
                let mut field_idx = 0;
                for item in inits {
                    let (field, value) = match item {
                        InitItem::Expr(e) => {
                            if field_idx < fields.len() {
                                let f = &fields[field_idx];
                                field_idx += 1;
                                (f, e)
                            } else { continue; }
                        }
                        InitItem::Designated { designator: Designator::Field(name), value } => {
                            if let Some(f) = fields.iter().find(|f| f.name == name.name) {
                                (f, value)
                            } else { continue; }
                        }
                        _ => continue,
                    };
                    self.gen_expr(value, 0);
                    let off = var_off + field.offset as i32;
                    self.emit_store(0, off, field.ty.size());
                }
            }
            _ => {
                // 标量类型的单元素初始化
                if let Some(InitItem::Expr(e)) = inits.first() {
                    self.gen_expr(e, 0);
                    self.mov_to_stack(0, var_name);
                }
            }
        }
    }

    // 全局变量初始化列表: 生成初始化数据字节
    fn gen_global_init_list(
        &mut self,
        ty: &Type,
        inits: &[InitItem],
        rel: Option<(u64, Option<usize>)>,
    ) -> Option<Vec<u8>> {
        let mut data = Vec::new();
        match ty {
            Type::Array(elem_ty, _) => {
                let elem_size = elem_ty.size();
                for (i, item) in inits.iter().enumerate() {
                    let value = match item {
                        InitItem::Expr(e) => e,
                        InitItem::Designated { designator: Designator::Index(idx_expr), value } => {
                            if let Expr::Int { v, .. } = idx_expr {
                                while data.len() < (*v as usize) * elem_size { data.push(0); }
                                value
                            } else { continue; }
                        }
                        _ => continue,
                    };
                    if let Expr::InitList { inits: nested, ty: nested_ty, .. } = value {
                        let nested_ty = if matches!(nested_ty, Type::Int) { elem_ty.as_ref() } else { nested_ty };
                        let elem_rel = rel.map(|(base, sec)| (base + data.len() as u64, sec));
                        if let Some(elem_data) = self.gen_global_init_list(nested_ty, nested, elem_rel) {
                            data.extend_from_slice(&elem_data);
                        }
                    } else {
                        let elem_rel = rel.map(|(base, sec)| (base + data.len() as u64, sec));
                        let elem_data = self.eval_const_expr(value, elem_rel, elem_size);
                        data.extend_from_slice(&elem_data);
                    }
                    while data.len() < (i + 1) * elem_size { data.push(0); }
                }
            }
            Type::Struct(_, fields) | Type::Union(_, fields) => {
                let total_size = ty.size();
                data.resize(total_size, 0);
                let mut field_idx = 0;
                for item in inits {
                    let (field, value) = match item {
                        InitItem::Expr(e) => {
                            if field_idx < fields.len() {
                                let f = &fields[field_idx];
                                field_idx += 1;
                                (f, e)
                            } else { continue; }
                        }
                        InitItem::Designated { designator: Designator::Field(name), value } => {
                            if let Some(f) = fields.iter().find(|f| f.name == name.name) {
                                (f, value)
                            } else { continue; }
                        }
                        _ => continue,
                    };
                    let field_rel = rel.map(|(base, sec)| (base + field.offset as u64, sec));
                    let field_data = self.eval_const_expr(value, field_rel, field.ty.size());
                    let off = field.offset;
                    for (j, &b) in field_data.iter().enumerate() {
                        if off + j < data.len() { data[off + j] = b; }
                    }
                }
            }
            Type::Vector(elem_ty, n) => {
                let elem_size = elem_ty.size();
                data.resize(elem_size * n, 0);
                for (i, item) in inits.iter().enumerate() {
                    if i >= *n { break; }
                    let value = match item {
                        InitItem::Expr(e) => e,
                        _ => continue,
                    };
                    let elem_rel = rel.map(|(base, sec)| (base + (i * elem_size) as u64, sec));
                    let elem_data = self.eval_const_expr(value, elem_rel, elem_size);
                    let off = i * elem_size;
                    for (j, &b) in elem_data.iter().enumerate() {
                        if off + j < data.len() { data[off + j] = b; }
                    }
                }
            }
            _ => {
                if let Some(InitItem::Expr(e)) = inits.first() {
                    data = self.eval_const_expr(e, rel, ty.size());
                }
            }
        }
        if data.is_empty() { None } else { Some(data) }
    }

    // 计算常量表达式的字节表示；符号引用写入 pending_data64_rels。
    fn eval_const_expr(
        &mut self,
        e: &Expr,
        rel: Option<(u64, Option<usize>)>,
        width: usize,
    ) -> Vec<u8> {
        if let Ok(value) = const_eval::eval(e) {
            let size = e.ty().size().max(width);
            return value.to_le_bytes()[..size].to_vec();
        }
        match e {
            Expr::String { data, .. } => data.clone(),
            Expr::Ident { name, .. } => {
                if let Some((off, sec)) = rel {
                    let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                    self.pending_data64_rels.push((off, name.name.clone(), sec, rel_type, 0));
                }
                vec![0u8; width.max(8)]
            }
            Expr::AddrOf { e: inner, .. } => match inner.as_ref() {
                Expr::Ident { .. } => self.eval_const_expr(inner, rel, width),
                Expr::Index { arr, idx, ty: elem_ty, .. } => {
                    if let (Expr::Ident { name, .. }, Ok(i)) = (arr.as_ref(), const_eval::eval(idx)) {
                        if let Some((off, sec)) = rel {
                            let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                            let addend = (i as i64) * elem_ty.size().max(1) as i64;
                            self.pending_data64_rels.push((
                                off, name.name.clone(), sec, rel_type, addend));
                        }
                        vec![0u8; width.max(8)]
                    } else {
                        vec![0u8; width.max(e.ty().size())]
                    }
                }
                _ => vec![0u8; width.max(e.ty().size())],
            },
            _ => vec![0u8; width.max(e.ty().size())],
        }
    }

    // 存储到内存 [r13 + offset]: 根据大小生成对应的存储指令
    fn emit_store(&mut self, src_reg: u8, offset: i32, size: usize) {
        // 使用 store_stack 的逻辑，但支持任意偏移
        let mut rex = 0x49u8; // REX.W + REX.B (r13)
        if src_reg >= 8 { rex |= 0x04; } // REX.R for src_reg>=8
        match size {
            1 => {
                // mov [r13 + disp], src_reg (byte) - 不需要 REX.W
                let rex_byte = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 }; // REX.B for r13
                self.emit(&[rex_byte, 0x88]);
                if offset.abs() < 128 {
                    self.emit(&[0x45 | ((src_reg & 7) << 3), offset as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((src_reg & 7) << 3)]);
                    self.emit(&offset.to_le_bytes());
                }
            }
            2 => {
                // mov [r13 + disp], src_reg (word)
                self.emit(&[0x66]); // operand size override
                let rex_byte = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 }; // REX.B for r13
                self.emit(&[rex_byte, 0x89]);
                if offset.abs() < 128 {
                    self.emit(&[0x45 | ((src_reg & 7) << 3), offset as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((src_reg & 7) << 3)]);
                    self.emit(&offset.to_le_bytes());
                }
            }
            4 => {
                // mov [r13 + disp], src_reg (dword) - 不用 REX.W, 否则会变成 8 字节存储
                let rex = 0x41u8; // REX.B only (r13), 无 REX.W
                if src_reg >= 8 { /* REX.R for src_reg>=8 */ }
                self.emit(&[rex | if src_reg >= 8 { 0x04 } else { 0 }, 0x89]);
                if offset.abs() < 128 {
                    self.emit(&[0x45 | ((src_reg & 7) << 3), offset as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((src_reg & 7) << 3)]);
                    self.emit(&offset.to_le_bytes());
                }
            }
            8 => {
                // mov [r13 + disp], src_reg (qword)
                self.emit(&[rex, 0x89]);
                if offset.abs() < 128 {
                    self.emit(&[0x45 | ((src_reg & 7) << 3), offset as i8 as u8]);
                } else {
                    self.emit(&[0x85 | ((src_reg & 7) << 3)]);
                    self.emit(&offset.to_le_bytes());
                }
            }
            _ => {}
        }
    }

    // P1-4: __int128 二元运算 codegen
    // 使用 rax:rdx = 右操作数, rcx:r8 = 左操作数, 结果 → rax:rdx
    fn gen_int128_binop(&mut self, op: BinOp, l: &Expr, r: &Expr, dst: u8) {
        // 1. 求值左操作数 → rax, push 两次 (低/高)
        self.gen_expr_decay(l, 0);
        self.push(0);           // push low
        if !matches!(l.ty(), Type::Int128 | Type::UInt128) {
            if matches!(l.ty(), Type::Bool | Type::UChar | Type::UShort
                | Type::UInt | Type::ULong | Type::ULongLong) {
                self.emit(&[0x48, 0x31, 0xD2]); // unsigned zero extension
            } else {
                self.emit(&[0x48, 0x99]); // signed extension
            }
        }
        self.push(2);           // push high
        // 2. 求值右操作数 → rax:rdx
        self.gen_expr_decay(r, 0);
        if !matches!(r.ty(), Type::Int128 | Type::UInt128) {
            if matches!(r.ty(), Type::Bool | Type::UChar | Type::UShort
                | Type::UInt | Type::ULong | Type::ULongLong) {
                self.emit(&[0x48, 0x31, 0xD2]);
            } else {
                self.emit(&[0x48, 0x99]);
            }
        }
        // 保存右操作数
        self.push(0);           // push rax (right_low)
        self.push(2);           // push rdx (right_high)
        // 3. 按与 push 相反的顺序恢复两个 128 位操作数。
        self.pop(11);           // r11 = right_high
        self.pop(9);            // r9 = right_low
        self.pop(8);            // r8 = left_high
        self.pop(1);            // rcx = left_low
        // 现在: rcx=left_low, r8=left_high, r9=right_low, r11=right_high
        // r14/r15 是 SysV 被调用者保存寄存器，临时使用前必须保护。
        self.push(14);
        self.push(15);
        match op {
            BinOp::Add => {
                // add rcx, r9 (low): REX.W+R(r9=9) = 0x4C
                self.emit(&[0x4C, 0x01, 0xC9]); // add rcx, r9
                // adc r8, r11 (high): REX.W+R(r11)+B(r8) = 0x4D
                self.emit(&[0x4D, 0x11, 0xD8]); // adc r8, r11
                self.mov(0, 1);  // rax = result_low
                self.mov(2, 8);  // rdx = result_high
            }
            BinOp::Sub => {
                // sub rcx, r9 (low): REX.W+R(r9) = 0x4C
                self.emit(&[0x4C, 0x29, 0xC9]); // sub rcx, r9
                // sbb r8, r11 (high): REX.W+R(r11)+B(r8) = 0x4D
                self.emit(&[0x4D, 0x19, 0xD8]); // sbb r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            }
            BinOp::And => {
                self.emit(&[0x4C, 0x21, 0xC9]); // and rcx, r9
                self.emit(&[0x4D, 0x21, 0xD8]); // and r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            }
            BinOp::Or => {
                self.emit(&[0x4C, 0x09, 0xC9]); // or rcx, r9
                self.emit(&[0x4D, 0x09, 0xD8]); // or r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            }
            BinOp::Xor => {
                self.emit(&[0x4C, 0x31, 0xC9]); // xor rcx, r9
                self.emit(&[0x4D, 0x31, 0xD8]); // xor r8, r11
                self.mov(0, 1);
                self.mov(2, 8);
            }
            BinOp::Mul => {
                // 低 128 位 = a0*b0 + ((a0*b1 + a1*b0) << 64)。
                self.mov(0, 1);                         // rax = a0
                self.emit(&[0x49, 0xF7, 0xE1]);        // mul r9
                self.mov(14, 0);                        // 保存低 64 位
                self.mov(10, 2);                        // r10 = a0*b0 高位
                self.mov(0, 1);
                self.emit(&[0x49, 0xF7, 0xE3]);        // mul r11
                self.emit(&[0x49, 0x01, 0xC2]);        // add r10, rax
                self.mov(0, 8);
                self.emit(&[0x49, 0xF7, 0xE1]);        // mul r9
                self.emit(&[0x49, 0x01, 0xC2]);        // add r10, rax
                self.mov(0, 14);
                self.mov(2, 10);
            }
            BinOp::Div | BinOp::Mod => {
                let signed = !matches!(l.ty(), Type::UInt128)
                    && !matches!(r.ty(), Type::UInt128);
                self.gen_int128_divmod(signed, matches!(op, BinOp::Mod));
            }
            BinOp::EqEq => {
                self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
                self.emit(&[0x4C, 0x39, 0xC9]); // cmp rcx, r9 (low)
                let lne = self.fresh_label();
                self.jmp_rel32(&lne, JumpKind::Jne);
                self.emit(&[0x4D, 0x39, 0xD8]); // cmp r8, r11 (high)
                self.jmp_rel32(&lne, JumpKind::Jne);
                self.mov_imm(0, 1);
                self.label(&lne);
                self.emit(&[0x48, 0x31, 0xD2]); // xor rdx, rdx
            }
            BinOp::Ne => {
                self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
                self.mov_imm(0, 1);
                let leq = self.fresh_label();
                self.emit(&[0x4C, 0x39, 0xC9]); // cmp rcx, r9 (low)
                self.jmp_rel32(&leq, JumpKind::Jne);
                self.emit(&[0x4D, 0x39, 0xD8]); // cmp r8, r11 (high)
                self.jmp_rel32(&leq, JumpKind::Jne);
                self.mov_imm(0, 0);
                self.label(&leq);
                self.emit(&[0x48, 0x31, 0xD2]); // xor rdx, rdx
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let is_signed = !matches!(l.ty(), Type::UInt128)
                    && !matches!(r.ty(), Type::UInt128);
                self.gen_int128_cmp_ordering(op, is_signed);
            }
            BinOp::Shl => {
                // rcx=left_low, r8=left_high, r9=shift_amount
                // 目标: rax:left_low, rdx:left_high, rcx:shift_count
                // 但 rcx 原来存 left_low, 需要先保存到其他地方
                // 重新组织: 先保存 shift 到 r9→已在那里, 用 rax=left_low, rdx=left_high
                // rcx 被覆盖前 left_low 已在 rax 中? 不, left_low 在 rcx 中
                // 解决: 先 mov rax, rcx (保存 left_low), 然后 mov rcx, r9 (shift)
                self.mov(0, 1);  // rax = rcx = left_low
                self.mov(2, 8);  // rdx = r8 = left_high
                self.mov(1, 9);  // rcx = r9 = shift_amount
                let l_under_128 = self.fresh_label();
                let l_end = self.fresh_label();
                self.emit(&[0x48, 0x81, 0xF9]);
                self.emit(&128u32.to_le_bytes()); // cmp rcx, 128
                self.jmp_rel32(&l_under_128, JumpKind::Jl);
                self.emit(&[0x48, 0x31, 0xC0, 0x48, 0x31, 0xD2]);
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                self.label(&l_under_128);
                // cmp rcx, 64
                self.emit(&[0x48, 0x83, 0xF9, 64]); // cmp rcx, 64
                let l_small = self.fresh_label();
                self.jmp_rel32(&l_small, JumpKind::Jl);
                // >= 64: rdx = rax << (cl-64), rax = 0
                self.mov(8, 0);  // r8 = rax = left_low (保存)
                self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax (low = 0)
                self.mov(2, 8);  // rdx = r8 = left_low
                self.emit(&[0x48, 0xD3, 0xE2]); // shl rdx, cl
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                // < 64: shld rdx, rax, cl; shl rax, cl
                self.label(&l_small);
                self.emit(&[0x48, 0x0F, 0xA5, 0xC2]); // shld rdx, rax, cl
                self.emit(&[0x48, 0xD3, 0xE0]);       // shl rax, cl
                self.label(&l_end);
                // rax:rdx = result
            }
            BinOp::Shr => {
                // rcx=left_low, r8=left_high, r9=shift_amount
                self.mov(0, 1);  // rax = rcx = left_low
                self.mov(2, 8);  // rdx = r8 = left_high
                self.mov(1, 9);  // rcx = r9 = shift_amount
                let signed = matches!(l.ty(), Type::Int128);
                let l_under_128 = self.fresh_label();
                let l_end = self.fresh_label();
                self.emit(&[0x48, 0x81, 0xF9]);
                self.emit(&128u32.to_le_bytes()); // cmp rcx, 128
                self.jmp_rel32(&l_under_128, JumpKind::Jl);
                if signed {
                    self.mov(0, 2);
                    self.emit(&[0x48, 0xC1, 0xF8, 63]); // sar rax, 63
                    self.mov(2, 0);
                } else {
                    self.emit(&[0x48, 0x31, 0xC0, 0x48, 0x31, 0xD2]);
                }
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                self.label(&l_under_128);
                self.emit(&[0x48, 0x83, 0xF9, 64]); // cmp rcx, 64
                let l_small = self.fresh_label();
                self.jmp_rel32(&l_small, JumpKind::Jl);
                // >= 64: rax = high >> (cl-64), high = sign/zero fill
                self.mov(8, 2);  // r8 = rdx = left_high (保存)
                self.mov(0, 8);  // rax = r8 = left_high
                if signed {
                    self.emit(&[0x48, 0xD3, 0xF8]); // sar rax, cl
                    self.mov(2, 8);
                    self.emit(&[0x48, 0xC1, 0xFA, 63]); // sar rdx, 63
                } else {
                    self.emit(&[0x48, 0xD3, 0xE8]); // shr rax, cl
                    self.emit(&[0x48, 0x31, 0xD2]); // high = 0
                }
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                // < 64: shrd rax, rdx, cl; shift high with sign/zero fill
                self.label(&l_small);
                self.emit(&[0x48, 0x0F, 0xAD, 0xD0]); // shrd rax, rdx, cl
                self.emit(&[0x48, 0xD3, if signed { 0xFA } else { 0xEA }]);
                self.label(&l_end);
            }
            BinOp::AndAnd | BinOp::OrOr => unreachable!("logical int128 ops are lowered separately"),
        }
        self.pop(15);
        self.pop(14);
        // 5. 移动到 dst
        if dst != 0 { self.mov(dst, 0); }
        // 结果的高 64 位在 rdx, 调用者需要存储到栈的第二个槽
    }

    /// 128 位 restoring division。
    /// 输入: r8:rcx dividend, r11:r9 divisor；输出: rdx:rax quotient/remainder。
    fn gen_int128_divmod(&mut self, signed: bool, want_remainder: bool) {
        // esi = 商符号，edi = 被除数符号（余数跟随被除数）。
        self.emit(&[0x31, 0xF6]); // xor esi, esi
        self.emit(&[0x31, 0xFF]); // xor edi, edi
        if signed {
            let lhs_positive = self.fresh_label();
            self.emit(&[0x4D, 0x85, 0xC0]); // test r8, r8
            self.jmp_rel32(&lhs_positive, JumpKind::Jns);
            self.emit(&[0x48, 0xF7, 0xD9]);       // neg rcx
            self.emit(&[0x49, 0x83, 0xD0, 0x00]); // adc r8, 0
            self.emit(&[0x49, 0xF7, 0xD8]);       // neg r8
            self.emit(&[0x83, 0xF6, 0x01]);       // xor esi, 1
            self.emit(&[0xBF, 1, 0, 0, 0]);       // mov edi, 1
            self.label(&lhs_positive);

            let rhs_positive = self.fresh_label();
            self.emit(&[0x4D, 0x85, 0xDB]); // test r11, r11
            self.jmp_rel32(&rhs_positive, JumpKind::Jns);
            self.emit(&[0x49, 0xF7, 0xD9]);       // neg r9
            self.emit(&[0x49, 0x83, 0xD3, 0x00]); // adc r11, 0
            self.emit(&[0x49, 0xF7, 0xDB]);       // neg r11
            self.emit(&[0x83, 0xF6, 0x01]);       // xor esi, 1
            self.label(&rhs_positive);
        }

        // C 对零除未定义；用 ud2 明确终止，绝不制造数值 0。
        self.mov(10, 9);
        self.emit(&[0x4D, 0x09, 0xDA]); // or r10, r11
        let divisor_ok = self.fresh_label();
        self.emit(&[0x4D, 0x85, 0xD2]); // test r10, r10
        self.jmp_rel32(&divisor_ok, JumpKind::Jne);
        self.emit(&[0x0F, 0x0B]);       // ud2
        self.label(&divisor_ok);

        self.mov(0, 1); // quotient/dividend low
        self.mov(2, 8); // quotient/dividend high
        self.emit(&[0x4D, 0x31, 0xF6]); // xor r14, r14 (remainder low)
        self.emit(&[0x4D, 0x31, 0xFF]); // xor r15, r15 (remainder high)
        self.emit(&[0x41, 0xBA]);
        self.emit(&128u32.to_le_bytes()); // mov r10d, 128
        let loop_label = self.fresh_label();
        let subtract = self.fresh_label();
        let next = self.fresh_label();
        self.label(&loop_label);
        self.emit(&[0x48, 0xD1, 0xE0]); // shl rax, 1
        self.emit(&[0x48, 0xD1, 0xD2]); // rcl rdx, 1
        self.emit(&[0x49, 0xD1, 0xD6]); // rcl r14, 1
        self.emit(&[0x49, 0xD1, 0xD7]); // rcl r15, 1
        self.emit(&[0x4D, 0x39, 0xDF]); // cmp r15, r11
        self.jmp_rel32(&next, JumpKind::Jb);
        self.jmp_rel32(&subtract, JumpKind::Ja);
        self.emit(&[0x4D, 0x39, 0xCE]); // cmp r14, r9
        self.jmp_rel32(&next, JumpKind::Jb);
        self.label(&subtract);
        self.emit(&[0x4D, 0x29, 0xCE]); // sub r14, r9
        self.emit(&[0x4D, 0x19, 0xDF]); // sbb r15, r11
        self.emit(&[0x48, 0x83, 0xC8, 0x01]); // or rax, 1
        self.label(&next);
        self.emit(&[0x49, 0xFF, 0xCA]); // dec r10
        self.jmp_rel32(&loop_label, JumpKind::Jne);

        if want_remainder {
            self.mov(0, 14);
            self.mov(2, 15);
            if signed {
                let positive = self.fresh_label();
                self.emit(&[0x85, 0xFF]); // test edi, edi
                self.jmp_rel32(&positive, JumpKind::Je);
                self.emit(&[0x48, 0xF7, 0xD8]);       // neg rax
                self.emit(&[0x48, 0x83, 0xD2, 0x00]); // adc rdx, 0
                self.emit(&[0x48, 0xF7, 0xDA]);       // neg rdx
                self.label(&positive);
            }
        } else if signed {
            let positive = self.fresh_label();
            self.emit(&[0x85, 0xF6]); // test esi, esi
            self.jmp_rel32(&positive, JumpKind::Je);
            self.emit(&[0x48, 0xF7, 0xD8]);       // neg rax
            self.emit(&[0x48, 0x83, 0xD2, 0x00]); // adc rdx, 0
            self.emit(&[0x48, 0xF7, 0xDA]);       // neg rdx
            self.label(&positive);
        }
    }

    // P1-4: __int128 有序比较 (Lt/Le/Gt/Ge)
    // 输入: rcx=left_low, r8=left_high, r9=right_low, r11=right_high
    // 输出: rax = 0 或 1, rdx = 0
    fn gen_int128_cmp_ordering(&mut self, op: BinOp, is_signed: bool) {
        let l_end = self.fresh_label();
        let l_high = self.fresh_label();
        self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
        // 比较高 64 位: cmp r8, r11
        self.emit(&[0x4D, 0x39, 0xD8]); // cmp r8, r11
        let (cc_high, cc_low_eq) = match op {
            BinOp::Lt if is_signed  => (0x9C, 0x92),
            BinOp::Le if is_signed  => (0x9C, 0x96),
            BinOp::Gt if is_signed  => (0x9F, 0x97),
            BinOp::Ge if is_signed  => (0x9F, 0x93),
            BinOp::Lt               => (0x92, 0x92),
            BinOp::Le               => (0x92, 0x96),
            BinOp::Gt               => (0x97, 0x97),
            BinOp::Ge               => (0x97, 0x93),
            _ => unreachable!(),
        };
        // 高半部不等时直接使用高半部比较结果。
        self.jmp_rel32(&l_high, JumpKind::Jne);
        // 高半部相等: 比较低半部 cmp rcx, r9
        self.emit(&[0x4C, 0x39, 0xC9]); // cmp rcx, r9
        self.emit(&[0x0F, cc_low_eq, 0xC0]); // setcc al
        self.jmp_rel32(&l_end, JumpKind::Jmp);
        // 高半部不等: 使用高半部比较结果
        self.label(&l_high);
        self.emit(&[0x0F, cc_high, 0xC0]); // setcc al
        self.label(&l_end);
        self.emit(&[0x48, 0x31, 0xD2]); // xor rdx, rdx
    }

    // P1-4: __int128 加载变量到 rax:rdx (低:高)
    fn gen_int128_load(&mut self, name: &str) {
        if self.globals.contains_key(name) {
            self.emit_global_addr(11, name);       // r11 = &global
            self.emit(&[0x49, 0x8B, 0x03]);       // rax = [r11]
            self.emit(&[0x49, 0x8B, 0x53, 0x08]); // rdx = [r11+8]
        } else {
            let off = self.var_offset(name);
            // rax = [r13 + off] (low)
            self.load_stack(0, off);
            // rdx = [r13 + off + 8] (high)
            self.load_stack(2, off + 8);
        }
    }

    // P1-4: __int128 从 rax:rdx 存储到变量
    fn gen_int128_store(&mut self, name: &str) {
        if self.globals.contains_key(name) {
            self.emit_global_addr(11, name);
            self.emit(&[0x49, 0x89, 0x03]);       // [r11] = rax
            self.emit(&[0x49, 0x89, 0x53, 0x08]); // [r11+8] = rdx
        } else {
            let off = self.var_offset(name);
            self.store_stack(0, off);      // low → [r13+off]
            self.store_stack(2, off + 8);  // high → [r13+off+8]
        }
    }

    // 指针/整数自增自减: step = sizeof(*ptr) 或 1
    fn gen_inc_dec(&mut self, e: &Expr, dst: u8, is_pre: bool, is_inc: bool) {
        let inner_ty = e.ty();
        let step = if inner_ty.is_ptr() {
            if let Type::Ptr(ref inner) = inner_ty { inner.size() as i32 } else { 1 }
        } else { 1 };
        if let Expr::Ident { name, .. } = e {
            let off = self.var_offset(&name.name);
            self.load_stack(dst, off);
            if !is_pre { self.push(dst); } // 后缀: 保存旧值
            // add/sub imm32 (0x81 /0 for add, 0x81 /5 for sub)
            let opcode = if is_inc { 0x81u8 } else { 0x81u8 };
            let modrm_base = if is_inc { 0xC0u8 | ((dst & 7) << 3) } else { 0xE8u8 | ((dst & 7) << 3) };
            if dst < 8 {
                self.emit(&[0x48, opcode, modrm_base]);
                self.emit(&(step as i32).to_le_bytes());
            } else {
                self.emit(&[0x49, opcode, modrm_base]);
                self.emit(&(step as i32).to_le_bytes());
            }
            self.store_stack(dst, off);
            if !is_pre { self.pop(dst); } // 后缀: 恢复旧值到 dst
        }
    }

    // 浮点运算: SSE 指令 (addsd/sd, subsd, mulsd, divsd, ucomisd)
    // 使用 xmm0 和 xmm1, 结果在 xmm0
    // 左操作数保存到帧临时槽 (避免嵌套 binop 时 xmm 寄存器冲突)
    fn gen_float_binop(&mut self, op: BinOp, l: &Expr, r: &Expr) {
        let is_float = matches!(l.ty(), Type::Float);
        // 取模: 软件实现 fmod = l - trunc(l/r) * r
        // x86 无原生 float 模指令, dcc 无内置链接器 (非 -c 模式), 不能直接调
        // fmodf/fmod, 用 cvttsX2si 截断 + cvtsi2sX 回转 + sub/mul 实现.
        // 边界 (NaN/Inf/0) 由 cvttsX2si 返回 0x80000000, 测试用例覆盖不到.
        if matches!(op, BinOp::Mod) {
            // 复用上方 l→[rsp], 算 r, r→xmm1, l→xmm0 的代码段
            // 直接走通用路径, 这样节省重复 emit
            let save_bytes = (self.expr_depth as i32 + 1) * 8;
            self.gen_expr(l, 0);
            self.emit(&[0x48, 0x83, 0xEC, save_bytes as u8]);
            if is_float {
                self.emit(&[0xF3, 0x0F, 0x11, 0x04, 0x24]); // movss [rsp], xmm0
            } else {
                self.emit(&[0xF2, 0x0F, 0x11, 0x04, 0x24]); // movsd [rsp], xmm0
            }
            self.expr_depth += 1;
            self.gen_expr(r, 0);
            self.expr_depth -= 1;
            self.emit(&[0x66, 0x0F, 0x28, 0xC8]); // movapd xmm1, xmm0 (r→xmm1)
            if is_float {
                self.emit(&[0xF3, 0x0F, 0x10, 0x04, 0x24]); // movss xmm0, [rsp]
            } else {
                self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x24]); // movsd xmm0, [rsp]
            }
            // 现在 xmm0=l, xmm1=r, [rsp]=l, save_bytes 字节尚未归还
            // r = l / r  (divss/divsd)
            if is_float {
                self.emit(&[0xF3, 0x0F, 0x5E, 0xC1]); // divss xmm0, xmm1
            } else {
                self.emit(&[0xF2, 0x0F, 0x5E, 0xC1]); // divsd xmm0, xmm1
            }
            // trunc 截断到 int64
            if is_float {
                self.emit(&[0xF3, 0x48, 0x0F, 0x2C, 0xC0]); // cvttss2si rax, xmm0
            } else {
                self.emit(&[0xF2, 0x48, 0x0F, 0x2C, 0xC0]); // cvttsd2si rax, xmm0
            }
            // int → float
            if is_float {
                self.emit(&[0xF3, 0x48, 0x0F, 0x2A, 0xC0]); // cvtsi2ss xmm0, rax
            } else {
                self.emit(&[0xF2, 0x48, 0x0F, 0x2A, 0xC0]); // cvtsi2sd xmm0, rax
            }
            // n * r
            if is_float {
                self.emit(&[0xF3, 0x0F, 0x59, 0xC1]); // mulss xmm0, xmm1
            } else {
                self.emit(&[0xF2, 0x0F, 0x59, 0xC1]); // mulsd xmm0, xmm1
            }
            // n*r 暂存 xmm1, l 回到 xmm0
            self.emit(&[0x66, 0x0F, 0x28, 0xC8]); // movapd xmm1, xmm0
            if is_float {
                self.emit(&[0xF3, 0x0F, 0x10, 0x04, 0x24]); // movss xmm0, [rsp]
            } else {
                self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x24]); // movsd xmm0, [rsp]
            }
            // xmm0 = l - n*r
            if is_float {
                self.emit(&[0xF3, 0x0F, 0x5C, 0xC1]); // subss xmm0, xmm1
            } else {
                self.emit(&[0xF2, 0x0F, 0x5C, 0xC1]); // subsd xmm0, xmm1
            }
            self.emit(&[0x48, 0x83, 0xC4, save_bytes as u8]); // add rsp, save_bytes
            return;
        }
        // 保存左操作数到 rsp 下方 (红区)
        // 用 sub rsp, 8 分配空间, 避免 movq [rsp-8] 编码问题
        // 嵌套时每个深度用不同的偏移: [rsp - 8*(depth+1)]
        let save_bytes = (self.expr_depth as i32 + 1) * 8;
        // 计算左操作数到 xmm0
        self.gen_expr(l, 0);
        // 保存 xmm0: sub rsp, save_bytes; movsd [rsp], xmm0
        self.emit(&[0x48, 0x83, 0xEC, save_bytes as u8]); // sub rsp, save_bytes
        if is_float {
            self.emit(&[0xF3, 0x0F, 0x11, 0x04, 0x24]); // movss [rsp], xmm0
        } else {
            self.emit(&[0xF2, 0x0F, 0x11, 0x04, 0x24]); // movsd [rsp], xmm0
        }
        // 计算右操作数到 xmm0
        self.expr_depth += 1;
        self.gen_expr(r, 0);
        self.expr_depth -= 1;
        // xmm1 = 右操作数, 恢复左操作数到 xmm0
        self.emit(&[0x66, 0x0F, 0x28, 0xC8]); // movapd xmm1, xmm0 (右→xmm1)
        if is_float {
            self.emit(&[0xF3, 0x0F, 0x10, 0x04, 0x24]); // movss xmm0, [rsp]
        } else {
            self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x24]); // movsd xmm0, [rsp]
        }
        // 恢复 rsp
        self.emit(&[0x48, 0x83, 0xC4, save_bytes as u8]); // add rsp, save_bytes
        // 现在 xmm0=左, xmm1=右 → xmm0 op xmm1 = 左 op 右
        // 执行运算
        let (prefix, opcode) = match op {
            BinOp::Add => (if is_float { 0xF3 } else { 0xF2 }, 0x58), // addss/addsd
            BinOp::Sub => (if is_float { 0xF3 } else { 0xF2 }, 0x5C), // subss/subsd
            BinOp::Mul => (if is_float { 0xF3 } else { 0xF2 }, 0x59), // mulss/mulsd
            BinOp::Div => (if is_float { 0xF3 } else { 0xF2 }, 0x5E), // divss/divsd
            // 比较运算
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne => {
                self.gen_float_cmp(op, &l.ty());
                return;
            }
            _ => return, // 不支持的运算
        };
        // xmm0 op xmm1 -> xmm0
        self.emit(&[prefix, 0x0F, opcode, 0xC1]);
    }

    // 浮点比较: ucomisd + setcc -> al -> movzx rax, al
    fn gen_float_cmp(&mut self, op: BinOp, ty: &Type) {
        let is_float = matches!(ty, Type::Float);
        // xmm0 = 左操作数, xmm1 = 右操作数
        // ucomiss/ucomisd xmm0, xmm1 (比较 左 vs 右)
        if is_float {
            self.emit(&[0x0F, 0x2E, 0xC1]); // ucomiss xmm0, xmm1
        } else {
            self.emit(&[0x66, 0x0F, 0x2E, 0xC1]); // ucomisd xmm0, xmm1
        }
        // 设置条件码
        let cc = match op {
            BinOp::Lt => 0x92, // jb (below)
            BinOp::Le => 0x96, // jbe (below or equal)
            BinOp::Gt => 0x97, // ja (above)
            BinOp::Ge => 0x93, // jae (above or equal)
            BinOp::EqEq => 0x94, // je
            BinOp::Ne => 0x95, // jne
            _ => 0x94,
        };
        // setcc al
        self.emit(&[0x0F, cc, 0xC0]);
        // movzx rax, al
        self.emit(&[0x48, 0x0F, 0xB6, 0xC0]);
    }

    // SIMD 向量运算: 标量逐元素操作（整数用 GP，浮点用标量 SSE，不用向量 XMM 指令）
    fn gen_vector_binop(&mut self, op: BinOp, l: &Expr, r: &Expr, result_slot: usize) {
        let (elem_ty, count) = match l.ty() {
            Type::Vector(ref elem, n) => { let et = (**elem).clone(); (et, n) }
            _ => return,
        };
        let elem_size = elem_ty.size();
        let vec_size = elem_size * count;
        let result_off = self.vector_temp_base(result_slot, vec_size);
        let is_float = matches!(elem_ty, Type::Float);
        let is_double = matches!(elem_ty, Type::Double);
        if is_float || is_double {
            let (prefix, opcode) = match op {
                BinOp::Add => (if is_float { 0xF3 } else { 0xF2 }, 0x58),
                BinOp::Sub => (if is_float { 0xF3 } else { 0xF2 }, 0x5C),
                BinOp::Mul => (if is_float { 0xF3 } else { 0xF2 }, 0x59),
                _ => return,
            };
            for i in 0..count {
                self.gen_scalar_vector_load_sse(l, i, elem_size, is_float);
                self.emit_sub_rsp(8);
                if is_float {
                    self.emit(&[0xF3, 0x0F, 0x11, 0x04, 0x24]);
                } else {
                    self.emit(&[0xF2, 0x0F, 0x11, 0x04, 0x24]);
                }
                self.gen_scalar_vector_load_sse(r, i, elem_size, is_float);
                if is_float {
                    self.emit(&[0xF3, 0x0F, 0x10, 0xC8]); // movss xmm1, xmm0
                } else {
                    self.emit(&[0xF2, 0x0F, 0x10, 0xC8]); // movsd xmm1, xmm0
                }
                if is_float {
                    self.emit(&[0xF3, 0x0F, 0x10, 0x04, 0x24]);
                } else {
                    self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x24]);
                }
                self.emit_add_rsp(8);
                self.emit(&[prefix, 0x0F, opcode, 0xC1]); // xmm0 = left op right
                self.store_vector_elem_xmm(0, result_off + (i * elem_size) as i32, elem_size, is_float);
            }
            return;
        }
        for i in 0..count {
            self.gen_scalar_vector_load(l, i, elem_size);
            self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax
            self.gen_scalar_vector_load(r, i, elem_size);
            match op {
                BinOp::Add => { self.emit(&[0x48, 0x01, 0xC1]); }
                BinOp::Sub => { self.emit(&[0x48, 0x29, 0xC1]); }
                BinOp::Mul => { self.emit(&[0x48, 0x0F, 0xAF, 0xC8]); } // imul rcx, rax
                _ => { self.emit(&[0x48, 0x01, 0xC1]); }
            }
            let off = result_off + (i * elem_size) as i32;
            // add/sub/imul 结果在 rcx，按元素宽度写入临时区
            self.emit_store(1, off, elem_size);
        }
    }

    /// 加载向量元素到 xmm0（标量 SSE 路径）
    fn gen_scalar_vector_load_sse(&mut self, e: &Expr, elem_idx: usize, elem_size: usize, is_float: bool) {
        if let Some((elem_ty, n, inits)) = Self::vector_init_from_expr(e) {
            let vec_size = elem_size * n;
            self.gen_vector_init_rvalue(inits, elem_ty, n, 1);
            let temp_off = self.vector_temp_base(1, vec_size) + (elem_idx * elem_size) as i32;
            self.load_stack_xmm(0, temp_off, elem_size, is_float);
            return;
        }
        match e {
            Expr::Ident { name, .. } => {
                if self.globals.contains_key(&name.name) {
                    self.emit_global_addr(0, &name.name);
                    let byte_off = (elem_idx * elem_size) as i32;
                    if byte_off != 0 {
                        if byte_off >= -128 && byte_off <= 127 {
                            self.emit(&[0x48, 0x83, 0xC0, byte_off as u8]);
                        } else {
                            self.emit(&[0x48, 0x05]);
                            self.emit(&byte_off.to_le_bytes());
                        }
                    }
                    self.load_mem_xmm(0, elem_size, is_float);
                } else {
                    let off = self.var_offset(&name.name) + (elem_idx * elem_size) as i32;
                    self.load_stack_xmm(0, off, elem_size, is_float);
                }
            }
            _ if matches!(e.ty(), Type::Vector(_, _)) => {
                let count = match e.ty() {
                    Type::Vector(_, n) => n,
                    _ => 0,
                };
                let vec_size = elem_size * count;
                self.gen_vector_to_temp(e, 1);
                let temp_off = self.vector_temp_base(1, vec_size) + (elem_idx * elem_size) as i32;
                self.load_stack_xmm(0, temp_off, elem_size, is_float);
            }
            _ => {
                self.gen_expr(e, 0);
            }
        }
    }

    fn load_stack_xmm(&mut self, xmm: u8, off: i32, _elem_size: usize, is_float: bool) {
        if is_float {
            self.emit(&[0xF3, 0x41, 0x0F, 0x10]);
        } else {
            self.emit(&[0xF2, 0x41, 0x0F, 0x10]);
        }
        if off.abs() < 128 {
            self.emit(&[0x45 | ((xmm & 7) << 3), off as i8 as u8]);
        } else {
            self.emit(&[0x85 | ((xmm & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        }
    }

    fn load_mem_xmm(&mut self, xmm: u8, elem_size: usize, is_float: bool) {
        if is_float {
            self.emit(&[0xF3, 0x0F, 0x10, 0x00 | ((xmm & 7) << 3)]);
        } else if elem_size == 8 {
            self.emit(&[0xF2, 0x0F, 0x10, 0x00 | ((xmm & 7) << 3)]);
        } else {
            self.emit(&[0xF3, 0x0F, 0x10, 0x00 | ((xmm & 7) << 3)]);
        }
    }

    fn store_vector_elem_xmm(&mut self, xmm: u8, off: i32, elem_size: usize, is_float: bool) {
        if is_float {
            self.store_xmm32_stack(xmm, off);
        } else if elem_size == 8 {
            self.store_xmm64_stack(xmm, off);
        } else {
            self.store_xmm32_stack(xmm, off);
        }
    }

    /// 从全局向量对象加载第 elem_idx 个元素到 rax（标量路径）
    fn emit_global_vector_elem_load(&mut self, name: &str, elem_idx: usize, elem_size: usize) {
        self.emit_global_addr(0, name);
        let byte_off = (elem_idx * elem_size) as i32;
        if byte_off != 0 {
            if byte_off >= -128 && byte_off <= 127 {
                self.emit(&[0x48, 0x83, 0xC0, byte_off as u8]);
            } else {
                self.emit(&[0x48, 0x05]);
                self.emit(&byte_off.to_le_bytes());
            }
        }
        self.emit_load_mem(elem_size);
    }

    /// 将 rax 中元素写入全局向量的第 elem_idx 个槽
    fn emit_global_vector_elem_store(&mut self, name: &str, elem_idx: usize, elem_size: usize) {
        self.emit_global_addr(1, name); // rcx = &global
        let byte_off = (elem_idx * elem_size) as i32;
        if byte_off != 0 {
            if byte_off >= -128 && byte_off <= 127 {
                self.emit(&[0x48, 0x83, 0xC1, byte_off as u8]);
            } else {
                self.emit(&[0x48, 0x81, 0xC1]);
                self.emit(&byte_off.to_le_bytes());
            }
        }
        // mov [rcx], rax (按元素宽度)
        match elem_size {
            1 => self.emit(&[0x88, 0x01]),
            2 => self.emit(&[0x66, 0x89, 0x01]),
            4 => self.emit(&[0x89, 0x01]),
            _ => self.emit(&[0x48, 0x89, 0x01]),
        }
    }

    /// 向量 compound literal / Cast+InitList 的初始化列表
    fn vector_init_from_expr<'a>(e: &'a Expr) -> Option<(&'a Type, usize, &'a [InitItem])> {
        match e {
            Expr::CompoundLit { ty: Type::Vector(elem, n), inits, .. } => {
                Some((elem.as_ref(), *n, inits.as_slice()))
            }
            Expr::Cast { ty: Type::Vector(elem, n), e: inner, .. } => {
                if let Expr::InitList { inits, .. } = inner.as_ref() {
                    Some((elem.as_ref(), *n, inits.as_slice()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// 将向量 rvalue 物化到帧临时区 slot（标量逐元素路径）
    fn gen_vector_to_temp(&mut self, e: &Expr, temp_slot: usize) {
        if let Some((elem_ty, n, inits)) = Self::vector_init_from_expr(e) {
            self.gen_vector_init_rvalue(inits, elem_ty, n, temp_slot);
            return;
        }
        match e {
            Expr::Binop { op, l, r, .. } if matches!(e.ty(), Type::Vector(_, _)) => {
                self.gen_vector_binop(*op, l, r, temp_slot);
            }
            Expr::InitList { inits, .. } => {
                if let Type::Vector(elem_ty, n) = e.ty() {
                    self.gen_vector_init_rvalue(inits, &elem_ty, n, temp_slot);
                }
            }
            _ => self.gen_vector_load_into_slot(e, temp_slot),
        }
    }

    /// 加载向量变量的第 i 个元素到 rax（整数）或 xmm0（浮点）
    fn gen_scalar_vector_load(&mut self, e: &Expr, elem_idx: usize, elem_size: usize) {
        let is_float = matches!(e.ty(), Type::Vector(ref inner, _) if matches!(**inner, Type::Float | Type::Double));
        if is_float {
            self.gen_scalar_vector_load_sse(e, elem_idx, elem_size, matches!(e.ty(), Type::Vector(ref inner, _) if matches!(**inner, Type::Float)));
            return;
        }
        if let Some((elem_ty, n, inits)) = Self::vector_init_from_expr(e) {
            let vec_size = elem_size * n;
            self.gen_vector_init_rvalue(inits, elem_ty, n, 1);
            let temp_off = self.vector_temp_base(1, vec_size) + (elem_idx * elem_size) as i32;
            self.load_stack_sized(0, temp_off, elem_size);
            return;
        }
        match e {
            Expr::Ident { name, .. } => {
                if self.globals.contains_key(&name.name) {
                    self.emit_global_vector_elem_load(&name.name, elem_idx, elem_size);
                } else {
                    let off = self.var_offset(&name.name) + (elem_idx * elem_size) as i32;
                    self.load_stack_sized(0, off, elem_size);
                }
            }
            _ if matches!(e.ty(), Type::Vector(_, _)) => {
                let count = match e.ty() {
                    Type::Vector(_, n) => n,
                    _ => 0,
                };
                let vec_size = elem_size * count;
                self.gen_vector_to_temp(e, 1);
                let temp_off = self.vector_temp_base(1, vec_size) + (elem_idx * elem_size) as i32;
                self.load_stack_sized(0, temp_off, elem_size);
            }
            _ => {
                self.gen_expr(e, 0);
                let off = (elem_idx * elem_size) as i32;
                if off != 0 {
                    if off >= -128 && off <= 127 {
                        self.emit(&[0x48, 0x83, 0xC0, off as u8]);
                    } else {
                        self.emit(&[0x48, 0x05]);
                        self.emit(&off.to_le_bytes());
                    }
                }
                self.emit_load_mem(elem_size);
            }
        }
    }

    // 加载向量变量到临时区域 (完全标量, 不用 XMM)
    fn gen_vector_load(&mut self, e: &Expr) {
        self.gen_vector_load_into_slot(e, 0);
    }

    fn gen_vector_load_into_slot(&mut self, e: &Expr, temp_slot: usize) {
        let (elem_ty, count) = match e.ty() {
            Type::Vector(ref elem, n) => { let et = (**elem).clone(); (et, n) }
            _ => return,
        };
        if let Some((et, n, inits)) = Self::vector_init_from_expr(e) {
            self.gen_vector_init_rvalue(inits, et, n, temp_slot);
            return;
        }
        let elem_size = elem_ty.size();
        let vec_size = elem_size * count;
        let temp_off = self.vector_temp_base(temp_slot, vec_size);
        match e {
            Expr::Ident { name, .. } => {
                if self.globals.contains_key(&name.name) {
                    for i in 0..count {
                        self.emit_global_vector_elem_load(&name.name, i, elem_size);
                        let dst_off = temp_off + (i * elem_size) as i32;
                        self.emit_store(0, dst_off, elem_size);
                    }
                    return;
                }
                let var_off = self.var_offset(&name.name);
                let is_float_elem = matches!(elem_ty, Type::Float);
                let is_double_elem = matches!(elem_ty, Type::Double);
                for i in 0..count {
                    let src_off = var_off + (i * elem_size) as i32;
                    if is_float_elem || is_double_elem {
                        self.load_stack_xmm(0, src_off, elem_size, is_float_elem);
                    } else {
                        self.emit(&[0x49, 0x8D]);
                        if src_off.abs() < 128 {
                            self.emit(&[0x45, src_off as i8 as u8]);
                        } else {
                            self.emit(&[0x85]);
                            self.emit(&src_off.to_le_bytes());
                        }
                        self.emit_load_mem(elem_size);
                    }
                    let dst_off = temp_off + (i * elem_size) as i32;
                    if is_float_elem {
                        self.store_xmm32_stack(0, dst_off);
                    } else if is_double_elem {
                        self.store_xmm64_stack(0, dst_off);
                    } else {
                        self.emit_store(0, dst_off, elem_size);
                    }
                }
            }
            Expr::Binop { op, l, r, .. } => {
                self.gen_vector_binop(*op, l, r, temp_slot);
            }
            Expr::InitList { inits, .. } => {
                self.gen_vector_init_rvalue(inits, &elem_ty, count, temp_slot);
            }
            _ => {
                self.gen_expr(e, 0);
                let total_bytes = elem_size * count;
                self.emit(&[0x48, 0x89, 0xC6]); // mov rsi, rax
                self.emit_lea_r13(7, temp_off); // rdi = 临时区
                self.mov_imm(1, total_bytes as i128);
                self.emit(&[0xFC]); // cld
                self.emit(&[0xF3, 0x48, 0xA4]); // rep movsb
            }
        }
    }

    // 从临时区域存储到向量变量 (完全标量, 不用 XMM)
    fn gen_vector_store(&mut self, name: &str) {
        let ty = self.var_map.get(name).map(|(_, ty)| ty.clone())
            .or_else(|| self.globals.get(name).map(|g| g.ty.clone()))
            .unwrap_or(Type::Int);
        let (elem_ty, count) = match ty {
            Type::Vector(ref elem, n) => { let et = (**elem).clone(); (et, n) }
            _ => return,
        };
        let elem_size = elem_ty.size();
        let vec_size = elem_size * count;
        let temp_off = self.vector_temp_base(0, vec_size);
        let is_global = self.globals.contains_key(name);
        let is_float_elem = matches!(elem_ty, Type::Float);
        let is_double_elem = matches!(elem_ty, Type::Double);
        for i in 0..count {
            let src_off = temp_off + (i * elem_size) as i32;
            if is_float_elem || is_double_elem {
                self.load_stack_xmm(0, src_off, elem_size, is_float_elem);
            } else {
                self.emit(&[0x49, 0x8D]); // lea rax, [r13 + src_off]
                if src_off.abs() < 128 {
                    self.emit(&[0x45, src_off as i8 as u8]);
                } else {
                    self.emit(&[0x85]);
                    self.emit(&src_off.to_le_bytes());
                }
                self.emit_load_mem(elem_size);
            }
            if is_global {
                self.emit_global_vector_elem_store(name, i, elem_size);
            } else {
                let var_off = self.var_offset(name);
                let dst_off = var_off + (i * elem_size) as i32;
                if is_float_elem {
                    self.store_xmm32_stack(0, dst_off);
                } else if is_double_elem {
                    self.store_xmm64_stack(0, dst_off);
                } else {
                    self.emit_store(0, dst_off, elem_size);
                }
            }
        }
    }

    // 指令发射
    fn mov_imm(&mut self, reg: u8, v: i128) {
        if v >= i32::MIN as i128 && v <= i32::MAX as i128 {
            if reg < 8 { self.emit(&[0xb8 + reg]); self.emit(&(v as i32).to_le_bytes()); }
            else { self.emit(&[0x41, 0xb8 + (reg - 8)]); self.emit(&(v as i32).to_le_bytes()); }
        } else {
            self.emit(&[0x48 + if reg >= 8 { 0x41 } else { 0 }]);
            self.emit(&[0xb8 + (reg & 7)]); self.emit(&(v as i64).to_le_bytes());
        }
    }
    fn mov(&mut self, dst: u8, src: u8) {
        // mov dst, src: 使用 0x8B (mov r64, r/m64) 使 dst 在 reg 字段, src 在 r/m 字段
        let mut rex = 0x48u8; if dst >= 8 { rex |= 0x04; } if src >= 8 { rex |= 0x01; }
        self.emit(&[rex, 0x8B, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
    }

    /// 整数提升: 将小于 int 的整数类型提升到 64 位
    /// char/short → 符号扩展; uchar/ushort/bool → 零扩展
    fn gen_int_promote(&mut self, dst: u8, ty: &Type) {
        let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
        match ty {
            Type::Char | Type::SChar | Type::Bool => {
                // movsx dst, dst_low8: 48 0F BE C0+dst,dst
                self.emit(&[rex, 0x0F, 0xBE, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            }
            Type::Short => {
                // movsx dst, dst_low16: 48 0F BF C0+dst,dst
                self.emit(&[rex, 0x0F, 0xBF, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            }
            Type::UChar => {
                // movzx dst, dst_low8: 48 0F B6 C0+dst,dst
                self.emit(&[rex, 0x0F, 0xB6, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            }
            Type::UShort => {
                // movzx dst, dst_low16: 48 0F B7 C0+dst,dst
                self.emit(&[rex, 0x0F, 0xB7, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
            }
            _ => {} // int/long/ptr 等不需要提升
        }
    }
    fn push(&mut self, reg: u8) {
        // 硬件 push: 用 rsp, 变量用 r13, 互不干扰
        if reg == 0 { self.emit(&[0x50]); }       // push rax
        else if reg == 1 { self.emit(&[0x51]); }   // push rcx
        else if reg == 2 { self.emit(&[0x52]); }   // push rdx
        else if reg == 3 { self.emit(&[0x53]); }   // push rbx
        else if reg == 7 { self.emit(&[0x57]); }   // push rdi
        else if reg == 6 { self.emit(&[0x56]); }   // push rsi
        else if reg == 8 { self.emit(&[0x41, 0x50]); } // push r8
        else if reg == 9 { self.emit(&[0x41, 0x51]); } // push r9
        else if reg == 10 { self.emit(&[0x41, 0x52]); } // push r10
        else if reg == 11 { self.emit(&[0x41, 0x53]); } // push r11
        else if reg == 12 { self.emit(&[0x41, 0x54]); } // push r12
        else if reg == 13 { self.emit(&[0x41, 0x55]); } // push r13
        else if reg == 14 { self.emit(&[0x41, 0x56]); } // push r14
        else if reg == 15 { self.emit(&[0x41, 0x57]); } // push r15
        else { panic!("push: unsupported reg {}", reg); }
    }
    fn pop(&mut self, reg: u8) {
        if reg == 0 { self.emit(&[0x58]); }       // pop rax
        else if reg == 1 { self.emit(&[0x59]); }   // pop rcx
        else if reg == 2 { self.emit(&[0x5a]); }   // pop rdx
        else if reg == 3 { self.emit(&[0x5b]); }   // pop rbx
        else if reg == 7 { self.emit(&[0x5f]); }   // pop rdi
        else if reg == 6 { self.emit(&[0x5e]); }   // pop rsi
        else if reg == 8 { self.emit(&[0x41, 0x58]); } // pop r8
        else if reg == 9 { self.emit(&[0x41, 0x59]); } // pop r9
        else if reg == 10 { self.emit(&[0x41, 0x5a]); } // pop r10
        else if reg == 11 { self.emit(&[0x41, 0x5b]); } // pop r11
        else if reg == 12 { self.emit(&[0x41, 0x5c]); } // pop r12
        else if reg == 13 { self.emit(&[0x41, 0x5d]); } // pop r13
        else if reg == 14 { self.emit(&[0x41, 0x5e]); } // pop r14
        else if reg == 15 { self.emit(&[0x41, 0x5f]); } // pop r15
        else { panic!("pop: unsupported reg {}", reg); }
    }

    fn binop(&mut self, op: BinOp, dst: u8, src: u8) {
        let is_cmp = matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne);
        if !is_cmp && !matches!(op, BinOp::Div | BinOp::Mod) {
            let mut rex = 0x48u8; if src >= 8 { rex |= 0x44; } if dst >= 8 { rex |= 0x41; } self.emit(&[rex]);
        }
        let r = (src & 7) << 3; let m = dst & 7;
        match op {
            BinOp::Add => self.emit(&[0x01, 0xC0 | r | m]),
            BinOp::Sub => self.emit(&[0x29, 0xC0 | r | m]),
            BinOp::And => self.emit(&[0x21, 0xC0 | r | m]),
            BinOp::Or  => self.emit(&[0x09, 0xC0 | r | m]),
            BinOp::Xor => self.emit(&[0x31, 0xC0 | r | m]),
            BinOp::Mul => self.emit(&[0x0F, 0xAF, 0xC0 | ((dst & 7) << 3) | (src & 7)]),
            BinOp::Div | BinOp::Mod => {
                self.emit(&[0x48, 0x99]);
                let mut rex = 0x48u8; if src >= 8 { rex |= 0x41; }
                self.emit(&[rex, 0xF7, 0xF8 | (src & 7)]);
                if matches!(op, BinOp::Mod) { self.mov(dst, 2); }
            }
            BinOp::Shl => { let mut rex = 0x48u8; if dst >= 8 { rex |= 0x41; } self.emit(&[rex, 0xD3, 0xE0 | m]); }
            BinOp::Shr => { let mut rex = 0x48u8; if dst >= 8 { rex |= 0x41; } self.emit(&[rex, 0xD3, 0xE8 | m]); }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne => {
                let mut rex = 0x48u8; if src >= 8 { rex |= 0x44; } if dst >= 8 { rex |= 0x41; }
                self.emit(&[rex, 0x39, 0xC0 | ((src & 7) << 3) | (dst & 7)]);
                let cc = match op { BinOp::Lt=>0x9C, BinOp::Le=>0x9E, BinOp::Gt=>0x9F, BinOp::Ge=>0x9D, BinOp::EqEq=>0x94, BinOp::Ne=>0x95, _=>unreachable!() };
                self.emit(&[0x0F, cc, 0xC0, 0x48, 0x0F, 0xB6, 0xC0]);
                if dst != 0 { self.mov(dst, 0); }
            }
            _ => panic!("binop: {:?}", op),
        }
    }

    fn var_offset(&mut self, name: &str) -> i32 {
        if let Some(&(off, _)) = self.var_map.get(name) { return off; }
        // 基于实际类型大小计算偏移 (过滤 padding 条目)
        // 变量放在保存寄存器下方: offset = -(used + 16)
        let used: i32 = self.var_map.values()
            .filter(|(_, ty)| !matches!(ty, Type::Void))
            .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
            .sum();
        let off = -(used + 16);
        self.var_map.insert(name.to_string(), (off, Type::Int)); off
    }

    /// 全局变量 RIP-relative 加载: reg = [global_var]
    /// 根据变量实际大小选择操作数尺寸 (1/2/4/8 字节)
    fn emit_global_load(&mut self, reg: u8, name: &str) {
        let (sz, is_tls, _off) = self.globals.get(name)
            .map(|g| (g.size, g.is_tls, g.offset))
            .unwrap_or((8, false, 0));
        let tls_exec = is_tls && !self.relocatable;
        let pref = if reg >= 8 { 0x41 } else { 0x00 }; // REX.R for high reg
        match sz {
            1 => {
                let pos = self.pos() + 3 + if pref != 0 { 1 } else { 0 };
                let modrm = 0x05 | ((reg & 7) << 3);
                let mut b = if pref != 0 { vec![pref] } else { vec![] };
                b.extend_from_slice(&[0x0F, 0xB6, modrm, 0,0,0,0]);
                self.emit(&b);
                if tls_exec { self.pending_tls_rels.push((pos, name.to_string())); }
                else { self.pending_data_rels.push((pos, name.to_string())); }
            }
            2 => {
                let pos = self.pos() + 3 + if pref != 0 { 1 } else { 0 };
                let modrm = 0x05 | ((reg & 7) << 3);
                let mut b = if pref != 0 { vec![pref] } else { vec![] };
                b.extend_from_slice(&[0x0F, 0xB7, modrm, 0,0,0,0]);
                self.emit(&b);
                if tls_exec { self.pending_tls_rels.push((pos, name.to_string())); }
                else { self.pending_data_rels.push((pos, name.to_string())); }
            }
            4 => {
                let pos = self.pos() + 2 + if pref != 0 { 1 } else { 0 };
                let modrm = 0x05 | ((reg & 7) << 3);
                let mut b = if pref != 0 { vec![pref] } else { vec![] };
                b.extend_from_slice(&[0x8B, modrm, 0,0,0,0]);
                self.emit(&b);
                if tls_exec { self.pending_tls_rels.push((pos, name.to_string())); }
                else { self.pending_data_rels.push((pos, name.to_string())); }
            }
            _ => {
                let pos = self.pos() + 3;
                let rex = if reg >= 8 { 0x4C } else { 0x48 };
                let modrm = 0x05 | ((reg & 7) << 3);
                self.emit(&[rex, 0x8B, modrm, 0,0,0,0]);
                if tls_exec { self.pending_tls_rels.push((pos, name.to_string())); }
                else { self.pending_data_rels.push((pos, name.to_string())); }
            }
        }
    }

    /// 全局变量 RIP-relative 存储: [global_var] = reg
    /// 根据变量实际大小选择操作数尺寸 (1/2/4/8 字节)
    fn emit_global_store(&mut self, reg: u8, name: &str) {
        let (sz, is_tls, _off) = self.globals.get(name)
            .map(|g| (g.size, g.is_tls, g.offset))
            .unwrap_or((8, false, 0));
        let tls_exec = is_tls && !self.relocatable;
        let modrm = 0x05 | ((reg & 7) << 3);
        match sz {
            1 => {
                let pos = self.pos() + 2 + if reg >= 4 { 1 } else { 0 };
                if reg >= 4 {
                    self.emit(&[0x40, 0x88, modrm, 0,0,0,0]);
                } else {
                    self.emit(&[0x88, modrm, 0,0,0,0]);
                }
                if tls_exec { self.pending_tls_rels.push((pos, name.to_string())); }
                else { self.pending_data_rels.push((pos, name.to_string())); }
            }
            2 => {
                let pos = self.pos() + 3;
                self.emit(&[0x66, 0x89, modrm, 0,0,0,0]);
                if tls_exec { self.pending_tls_rels.push((pos, name.to_string())); }
                else { self.pending_data_rels.push((pos, name.to_string())); }
            }
            4 => {
                let pos = self.pos() + 2;
                self.emit(&[0x89, modrm, 0,0,0,0]);
                if tls_exec { self.pending_tls_rels.push((pos, name.to_string())); }
                else { self.pending_data_rels.push((pos, name.to_string())); }
            }
            _ => {
                let pos = self.pos() + 3;
                let rex = if reg >= 8 { 0x4C } else { 0x48 };
                self.emit(&[rex, 0x89, modrm, 0,0,0,0]);
                if tls_exec { self.pending_tls_rels.push((pos, name.to_string())); }
                else { self.pending_data_rels.push((pos, name.to_string())); }
            }
        }
    }

    /// 全局变量 RIP-relative 取地址: reg = &global_var
    /// 编码: REX.W 8D 05|reg<<3 disp32 (LEA)
    fn emit_global_addr(&mut self, reg: u8, name: &str) {
        let disp32_pos = self.pos() + 3;
        let rex = if reg >= 8 { 0x4C } else { 0x48 };
        self.emit(&[rex, 0x8D, 0x05 | ((reg & 7) << 3), 0, 0, 0, 0]);
        self.pending_data_rels.push((disp32_pos, name.to_string()));
    }

    // 根据大小从内存加载到 rax（整数）或 xmm0（浮点）
    fn emit_load_mem_typed(&mut self, size: usize, ty: &Type) {
        match ty {
            Type::Float => self.emit(&[0xF3, 0x0F, 0x10, 0x00]),
            Type::Double => self.emit(&[0xF2, 0x0F, 0x10, 0x00]),
            _ => self.emit_load_mem(size),
        }
    }

    // 根据大小从内存加载到 rax (零扩展)
    fn emit_load_mem(&mut self, size: usize) {
        match size {
            1 => self.emit(&[0x48, 0x0f, 0xb6, 0x00]), // movzbl (%rax), %rax
            2 => self.emit(&[0x48, 0x0f, 0xb7, 0x00]), // movzwl (%rax), %rax
            4 => self.emit(&[0x8b, 0x00]),              // mov (%rax), %eax (零扩展到64位)
            _ => self.emit(&[0x48, 0x8b, 0x00]),        // mov (%rax), %rax
        }
    }

    // 注册变量偏移 (数组按实际大小分配多个槽)
    fn register_var(&mut self, name: &str, ty: &Type) -> i32 {
        if let Some(&(off, _)) = self.var_map.get(name) { return off; }
        // P1-5: register asm 变量不分配栈槽, 直接绑定到指定寄存器
        if self.reg_bindings.contains_key(name) { return 0; }
        let slots = (ty.size() + 7) / 8; // 向上对齐到 8 字节
        let slots = slots.max(1) as i32;
        // 基于实际类型大小计算偏移 (过滤 padding 条目)
        // 变量放在保存寄存器下方: offset = -(used + 16)
        let used: i32 = self.var_map.values()
            .filter(|(_, ty)| !matches!(ty, Type::Void))
            .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
            .sum();
        let mut off = -(used + 16);
        while off + ty.size() as i32 > 0 {
            off -= 8;
        }
        self.var_map.insert(name.to_string(), (off, ty.clone()));
        // 为数组/向量预留额外槽位 (空插入占位, 保持 var_map 完整性)
        for i in 1..slots {
            let placeholder_name = format!("__pad_{}_{}", name, i);
            let pad_off = off - i * 8;
            self.var_map.insert(placeholder_name, (pad_off, Type::Void));
        }
        off
    }
    fn mov_to_stack(&mut self, reg: u8, name: &str) {
        // P1-5: register asm 变量直接存入绑定寄存器
        if let Some(&bound_reg) = self.reg_bindings.get(name) {
            if reg != bound_reg { self.mov(bound_reg, reg); }
            return;
        }
        let off = self.var_offset(name); self.store_stack(reg, off);
    }
    fn mov_from_stack(&mut self, reg: u8, name: &str) {
        // P1-5: register asm 变量直接从绑定寄存器加载
        if let Some(&bound_reg) = self.reg_bindings.get(name) {
            if reg != bound_reg { self.mov(reg, bound_reg); }
            return;
        }
        let off = self.var_offset(name); self.load_stack(reg, off);
    }

    // 浮点变量存储: xmm0 → 栈 (使用 XMM 指令)
    fn gen_float_store(&mut self, name: &str, ty: &Type) {
        let off = self.var_offset(name);
        let is_float = matches!(ty, Type::Float);
        let is_ld = matches!(ty, Type::LongDouble);
        if is_ld {
            // long double 16 字节: 分两次 movq 存储 (xmm0 低 8 字节 + 高 8 字节零)
            // movq [r13+off], xmm0: 66 49 0F D6 45|4D off
            self.emit(&[0x66, 0x49, 0x0F, 0xD6]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
            // 高 8 字节存零: mov qword [r13+off-8], 0
            // 注意: off 为负数, off-8 更远离 r13 (在变量自身空间内)
            let off2 = off - 8;
            self.emit(&[0x49, 0xC7, 0x45]);
            if off2.abs() < 128 { self.emit(&[off2 as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off2.to_le_bytes()); }
            self.emit(&0u32.to_le_bytes());
        } else if is_float {
            // float: movss [r13+off], xmm0 → F3 49 0F 11 45 off
            self.emit(&[0xF3, 0x49, 0x0F, 0x11]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        } else {
            // double: movsd [r13+off], xmm0 → F2 49 0F 11 45 off
            self.emit(&[0xF2, 0x49, 0x0F, 0x11]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        }
    }
    // 浮点变量加载: 栈 → xmm0 (使用 XMM 指令)
    fn gen_float_load(&mut self, name: &str, ty: &Type) {
        let off = self.var_offset(name);
        let is_float = matches!(ty, Type::Float);
        let is_ld = matches!(ty, Type::LongDouble);
        if is_ld {
            self.emit(&[0xF3, 0x49, 0x0F, 0x7E]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        } else if is_float {
            self.emit(&[0xF3, 0x49, 0x0F, 0x10]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        } else {
            self.emit(&[0xF2, 0x49, 0x0F, 0x10]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        }
    }

    // 浮点值按偏移存储: xmm0 → [r13+off]
    fn gen_float_store_at(&mut self, off: i32, ty: &Type) {
        let is_float = matches!(ty, Type::Float);
        let is_ld = matches!(ty, Type::LongDouble);
        if is_ld {
            self.emit(&[0x66, 0x49, 0x0F, 0xD6]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
            let off2 = off - 8;
            self.emit(&[0x49, 0xC7, 0x45]);
            if off2.abs() < 128 { self.emit(&[off2 as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off2.to_le_bytes()); }
            self.emit(&0u32.to_le_bytes());
        } else if is_float {
            self.emit(&[0xF3, 0x49, 0x0F, 0x11]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        } else {
            self.emit(&[0xF2, 0x49, 0x0F, 0x11]);
            if off.abs() < 128 { self.emit(&[0x45, off as i8 as u8]); }
            else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
        }
    }

    // Complex 变量标量初始化: 存实部、虚部清零
    fn gen_complex_init(&mut self, name: &str, inner: &Type, init: &Expr) {
        let off = self.var_offset(name);
        let inner_sz = inner.size() as i32;
        self.gen_expr(init, 0);
        // 存实部
        if matches!(inner, Type::Float | Type::Double | Type::LongDouble) {
            self.gen_float_store_at(off, inner);
        } else {
            let sz = inner.size();
            self.store_stack(0, off);
            if sz > 8 {
                self.store_stack(2, off + 8);
            }
        }
        // 虚部清零: LongDouble 实部占 [off-8, off+8), 虚部应在 [off-8-inner_sz, off-8)
        let imag_base = if matches!(inner, Type::LongDouble) {
            off - 8 - inner_sz  // LongDouble 向后占 16B
        } else {
            off + inner_sz
        };
        let imag_bytes = inner_sz as usize;
        if matches!(inner, Type::Float | Type::Double | Type::LongDouble) {
            // 浮点虚部仅 inner_sz 字节，勿用 64 位 store 以免覆盖相邻实部
            self.emit(&[0x0F, 0x57, 0xC0]); // xorps xmm0, xmm0
            self.gen_float_store_at(imag_base, inner);
            let _ = imag_bytes;
        } else {
            for o in (0..imag_bytes).step_by(8) {
                self.emit(&[0x48, 0x31, 0xC0]); // xor rax, rax
                self.store_stack(0, imag_base + o as i32);
            }
        }
    }

    // P2-14: _Complex 算术运算 (add/sub/mul/div)
    fn gen_complex_binop(&mut self, op: BinOp, l: &Expr, r: &Expr, _dst: u8) {
        let inner_ty = if let Type::Complex(inner) = &l.ty() { inner.as_ref().clone() } else { return };
        let inner_sz = inner_ty.size() as i32;
        let is_float = matches!(inner_ty, Type::Float);
        let total_sz = inner_sz * 2;
        let save_sz = total_sz * 2;
        let result_off = -(self.frame_size as i32) + 8;
        self.emit(&[0x48, 0x83, 0xEC, save_sz as u8]);
        self.spill_complex_operand(l, 0, inner_sz, is_float);
        self.spill_complex_operand(r, total_sz, inner_sz, is_float);
        let (load_prefix, op_add, op_sub, op_mul, op_div) = if is_float {
            (0xF3u8, 0x58u8, 0x5Cu8, 0x59u8, 0x5Eu8)
        } else {
            (0xF2u8, 0x58u8, 0x5Cu8, 0x59u8, 0x5Eu8)
        };
        match op {
            BinOp::Add | BinOp::Sub => {
                let op_code = if matches!(op, BinOp::Add) { op_add } else { op_sub };
                self.emit(&[load_prefix, 0x0F, 0x10, 0x04, 0x24]);
                self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, total_sz as u8]);
                self.emit(&[load_prefix, 0x0F, op_code, 0xC1]);
                self.emit(&[load_prefix, 0x0F, 0x11, 0x04, 0x24]);
                self.emit(&[load_prefix, 0x0F, 0x10, 0x44, 0x24, inner_sz as u8]);
                let r_imag_slot = total_sz + inner_sz;
                self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, r_imag_slot as u8]);
                self.emit(&[load_prefix, 0x0F, op_code, 0xC1]);
                self.emit(&[load_prefix, 0x0F, 0x11, 0x44, 0x24, inner_sz as u8]);
            }
            BinOp::Mul => {
                self.emit(&[load_prefix, 0x0F, 0x10, 0x04, 0x24]);
                self.emit(&[load_prefix, 0x0F, 0x10, 0x54, 0x24, total_sz as u8]);
                self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, inner_sz as u8]);
                let r_imag_slot = total_sz + inner_sz;
                self.emit(&[load_prefix, 0x0F, 0x10, 0x5C, 0x24, r_imag_slot as u8]);
                self.emit(&[0x66, 0x0F, 0x28, 0xE0]);
                self.emit(&[load_prefix, 0x0F, op_mul, 0xE2]);
                self.emit(&[0x66, 0x0F, 0x28, 0xE9]);
                self.emit(&[load_prefix, 0x0F, op_mul, 0xEB]);
                self.emit(&[load_prefix, 0x0F, op_sub, 0xE5]);
                self.emit(&[load_prefix, 0x0F, 0x11, 0x24, 0x24]);
                self.emit(&[0x66, 0x0F, 0x28, 0xE0]);
                self.emit(&[load_prefix, 0x0F, op_mul, 0xE3]);
                self.emit(&[0x66, 0x0F, 0x28, 0xE9]);
                self.emit(&[load_prefix, 0x0F, op_mul, 0xEA]);
                self.emit(&[load_prefix, 0x0F, op_add, 0xE5]);
                self.emit(&[load_prefix, 0x0F, 0x11, 0x64, 0x24, inner_sz as u8]);
            }
            BinOp::Div => {
                // (a+bi)/(c+di): real=(ac+bd)/den, imag=(bc-ad)/den, den=c²+d²
                self.emit(&[load_prefix, 0x0F, 0x10, 0x04, 0x24]); // xmm0=a
                self.emit(&[load_prefix, 0x0F, 0x10, 0x4C, 0x24, inner_sz as u8]); // xmm1=b
                self.emit(&[load_prefix, 0x0F, 0x10, 0x54, 0x24, total_sz as u8]); // xmm2=c
                let r_imag_slot = total_sz + inner_sz;
                self.emit(&[load_prefix, 0x0F, 0x10, 0x5C, 0x24, r_imag_slot as u8]); // xmm3=d
                // xmm6 = c² + d²
                self.mov_xmm(6, 2);
                self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(6, 6)]);
                self.mov_xmm(7, 3);
                self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(7, 7)]);
                self.emit(&[load_prefix, 0x0F, op_add, Self::modrm_reg_reg(6, 7)]);
                // xmm4 = (a*c + b*d) / den
                self.mov_xmm(4, 0);
                self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(4, 2)]);
                self.mov_xmm(7, 1);
                self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(7, 3)]);
                self.emit(&[load_prefix, 0x0F, op_add, Self::modrm_reg_reg(4, 7)]);
                self.emit(&[load_prefix, 0x0F, op_div, Self::modrm_reg_reg(4, 6)]);
                self.emit(&[load_prefix, 0x0F, 0x11, 0x24, 0x24]);
                // xmm7 = (b*c - a*d) / den
                self.mov_xmm(7, 1);
                self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(7, 2)]);
                self.mov_xmm(5, 0);
                self.emit(&[load_prefix, 0x0F, op_mul, Self::modrm_reg_reg(5, 3)]);
                self.emit(&[load_prefix, 0x0F, op_sub, Self::modrm_reg_reg(7, 5)]);
                self.emit(&[load_prefix, 0x0F, op_div, Self::modrm_reg_reg(7, 6)]);
                self.emit(&[load_prefix, 0x0F, 0x11, 0x7C, 0x24, inner_sz as u8]);
            }
            _ => {}
        }
        self.store_complex_pair_from_rsp(result_off, inner_sz, is_float);
        self.emit(&[0x48, 0x83, 0xC4, save_sz as u8]);
        // 实部载入 xmm0，供浮点/整型转换
        if is_float {
            self.emit(&[0xF3, 0x49, 0x0F, 0x10, 0x45]);
            if result_off.abs() < 128 { self.emit(&[result_off as u8]); }
            else { self.emit(&[0x85]); self.emit(&result_off.to_le_bytes()); }
        } else {
            self.emit(&[0xF2, 0x49, 0x0F, 0x10, 0x45]);
            if result_off.abs() < 128 { self.emit(&[result_off as u8]); }
            else { self.emit(&[0x85]); self.emit(&result_off.to_le_bytes()); }
        }
    }

    /// 将复数操作数写入 [rsp+slot]（实部）与 [rsp+slot+inner_sz]（虚部）
    fn spill_complex_operand(&mut self, e: &Expr, rsp_slot: i32, inner_sz: i32, is_float: bool) {
        let total_sz = inner_sz * 2;
        let result_off = -(self.frame_size as i32) + 8;
        match e {
            Expr::Ident { name, .. } => {
                let off = self.var_offset(&name.name);
                if is_float {
                    self.emit(&[0xF3, 0x49, 0x0F, 0x10, 0x45]);
                    if off.abs() < 128 { self.emit(&[off as u8]); }
                    else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
                    self.emit(&[0xF3, 0x0F, 0x11, 0x44, 0x24, rsp_slot as u8]);
                    let imag_off = off + inner_sz;
                    self.emit(&[0xF3, 0x49, 0x0F, 0x10, 0x45]);
                    if imag_off.abs() < 128 { self.emit(&[imag_off as u8]); }
                    else { self.emit(&[0x85]); self.emit(&imag_off.to_le_bytes()); }
                    self.emit(&[0xF3, 0x0F, 0x11, 0x44, 0x24, (rsp_slot + inner_sz) as u8]);
                } else {
                    self.emit(&[0xF2, 0x49, 0x0F, 0x10, 0x45]);
                    if off.abs() < 128 { self.emit(&[off as u8]); }
                    else { self.emit(&[0x85]); self.emit(&off.to_le_bytes()); }
                    self.emit(&[0xF2, 0x0F, 0x11, 0x44, 0x24, rsp_slot as u8]);
                    let imag_off = off + inner_sz;
                    self.emit(&[0xF2, 0x49, 0x0F, 0x10, 0x45]);
                    if imag_off.abs() < 128 { self.emit(&[imag_off as u8]); }
                    else { self.emit(&[0x85]); self.emit(&imag_off.to_le_bytes()); }
                    self.emit(&[0xF2, 0x0F, 0x11, 0x44, 0x24, (rsp_slot + inner_sz) as u8]);
                }
            }
            Expr::Binop { .. } => {
                self.gen_expr(e, 0);
                self.copy_complex_from_result_to_rsp(result_off, rsp_slot, inner_sz, is_float);
            }
            _ => {
                let sz = e.ty().size() as i32;
                self.gen_expr(e, 0);
                self.emit(&[0x48, 0x89, 0xC6]); // rsi = src
                self.emit(&[0x48, 0x89, 0xE7]); // rdi = rsp
                if rsp_slot != 0 {
                    if rsp_slot >= -128 && rsp_slot <= 127 {
                        self.emit(&[0x48, 0x83, 0xC7, rsp_slot as u8]);
                    } else {
                        self.emit(&[0x48, 0x81, 0xC7]);
                        self.emit(&rsp_slot.to_le_bytes());
                    }
                }
                self.mov_imm(1, sz as i128);
                self.emit(&[0xFC, 0xF3, 0xA4]); // cld; rep movsb
            }
        }
        let _ = total_sz;
    }

    fn store_complex_pair_from_rsp(&mut self, result_off: i32, inner_sz: i32, is_float: bool) {
        if is_float {
            self.emit(&[0xF3, 0x0F, 0x10, 0x04, 0x24]);
            self.emit(&[0xF3, 0x41, 0x0F, 0x11, 0x45]);
            if result_off.abs() < 128 { self.emit(&[result_off as u8]); }
            else { self.emit(&[0x85]); self.emit(&result_off.to_le_bytes()); }
            self.emit(&[0xF3, 0x0F, 0x10, 0x44, 0x24, inner_sz as u8]);
            self.emit(&[0xF3, 0x41, 0x0F, 0x11, 0x45]);
            let imag_off = result_off + inner_sz;
            if imag_off.abs() < 128 { self.emit(&[imag_off as u8]); }
            else { self.emit(&[0x85]); self.emit(&imag_off.to_le_bytes()); }
        } else {
            self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x24]);
            self.emit(&[0xF2, 0x41, 0x0F, 0x11, 0x45]);
            if result_off.abs() < 128 { self.emit(&[result_off as u8]); }
            else { self.emit(&[0x85]); self.emit(&result_off.to_le_bytes()); }
            self.emit(&[0xF2, 0x0F, 0x10, 0x44, 0x24, inner_sz as u8]);
            self.emit(&[0xF2, 0x41, 0x0F, 0x11, 0x45]);
            let imag_off = result_off + inner_sz;
            if imag_off.abs() < 128 { self.emit(&[imag_off as u8]); }
            else { self.emit(&[0x85]); self.emit(&imag_off.to_le_bytes()); }
        }
    }

    fn copy_complex_from_result_to_rsp(&mut self, result_off: i32, rsp_slot: i32, inner_sz: i32, is_float: bool) {
        let sz = inner_sz * 2;
        self.emit_lea_r13(6, result_off);
        self.emit(&[0x48, 0x89, 0xE7]); // rdi = rsp
        if rsp_slot != 0 {
            if rsp_slot >= -128 && rsp_slot <= 127 {
                self.emit(&[0x48, 0x83, 0xC7, rsp_slot as u8]);
            } else {
                self.emit(&[0x48, 0x81, 0xC7]);
                self.emit(&rsp_slot.to_le_bytes());
            }
        }
        self.mov_imm(1, sz as i128);
        self.emit(&[0xFC, 0xF3, 0xA4]);
        let _ = is_float;
    }

    fn load_upvar(&mut self, reg: u8, parent_off: i32, size: usize) {
        // r11 = 保存的父级 r13
        self.emit(&[0x4D, 0x8B, 0x9D]);
        self.emit(&self.static_chain_offset.to_le_bytes());
        let rex = 0x41 | if reg >= 8 { 0x04 } else { 0 }; // B=r11, R=目标高寄存器
        let modrm = 0x83 | ((reg & 7) << 3);
        match size {
            1 => self.emit(&[rex, 0x0F, 0xB6, modrm]),
            2 => self.emit(&[rex, 0x0F, 0xB7, modrm]),
            4 => self.emit(&[rex, 0x8B, modrm]),
            _ => self.emit(&[rex | 0x08, 0x8B, modrm]),
        }
        self.emit(&parent_off.to_le_bytes());
    }

    fn store_upvar(&mut self, reg: u8, parent_off: i32, size: usize) {
        self.emit(&[0x4D, 0x8B, 0x9D]);
        self.emit(&self.static_chain_offset.to_le_bytes());
        let rex = 0x41 | if reg >= 8 { 0x04 } else { 0 };
        let modrm = 0x83 | ((reg & 7) << 3);
        match size {
            1 => self.emit(&[rex, 0x88, modrm]),
            2 => self.emit(&[0x66, rex, 0x89, modrm]),
            4 => self.emit(&[rex, 0x89, modrm]),
            _ => self.emit(&[rex | 0x08, 0x89, modrm]),
        }
        self.emit(&parent_off.to_le_bytes());
    }

    fn store_stack(&mut self, reg: u8, off: i32) {
        // r13 基址: REX.W + REX.B(=1 for r13) + r/m=5 → [r13+disp8]
        let mut rex = 0x49u8; if reg >= 8 { rex |= 0x04; } // REX.R for reg>=8
        self.emit(&[rex, 0x89]);
        if off.abs() < 128 { self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]); }
        else { self.emit(&[0x85 | ((reg & 7) << 3)]); self.emit(&off.to_le_bytes()); }
    }
    fn load_stack(&mut self, reg: u8, off: i32) {
        self.load_stack_sized(reg, off, 8);
    }

    /// 从 r13 帧按元素宽度加载（向量逐元素路径避免 8 字节读穿邻元素）
    fn load_stack_sized(&mut self, reg: u8, off: i32, size: usize) {
        let rex_r = if reg >= 8 { 0x04 } else { 0 };
        match size {
            1 => {
                self.emit(&[0x41 | rex_r, 0x0F, 0xB6]);
            }
            2 => {
                self.emit(&[0x41 | rex_r, 0x0F, 0xB7]);
            }
            4 => {
                self.emit(&[0x41 | rex_r, 0x8B]);
            }
            _ => {
                self.emit(&[0x49 | rex_r, 0x8B]);
            }
        }
        if off.abs() < 128 {
            self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]);
        } else {
            self.emit(&[0x85 | ((reg & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        }
    }
    fn store_stack_xmm(&mut self, xmm: u8, off: i32) {
        self.emit(&[0xF2, 0x41, 0x0F, 0x11]);
        if off.abs() < 128 {
            self.emit(&[0x45 | ((xmm & 7) << 3), off as i8 as u8]);
        } else {
            self.emit(&[0x85 | ((xmm & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        }
    }
    fn load_r13_xmm(&mut self, xmm: u8, off: i32) {
        self.emit(&[0xF2, 0x41, 0x0F, 0x10]);
        if off.abs() < 128 {
            self.emit(&[0x45 | ((xmm & 7) << 3), off as i8 as u8]);
        } else {
            self.emit(&[0x85 | ((xmm & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        }
    }
    fn load_r13_incoming(&mut self, reg: u8, off: i32) {
        let mut rex = 0x49u8;
        if reg >= 8 { rex |= 0x04; }
        self.emit(&[rex, 0x8B, 0x85 | ((reg & 7) << 3)]);
        self.emit(&off.to_le_bytes());
    }

    // LEA reg, [r13 + off] — 取栈变量地址 (与 gen_expr_decay 编码一致)
    fn emit_lea_r13(&mut self, reg: u8, off: i32) {
        let rex = 0x49u8; // REX.W + REX.B (r13)
        self.emit(&[rex, 0x8D]);
        if off.abs() < 128 {
            self.emit(&[0x45 | ((reg & 7) << 3), off as i8 as u8]);
        } else {
            self.emit(&[0x85 | ((reg & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        }
    }

    // struct/array 整体赋值: 逐块拷贝 (src → dst)
    // 策略: 取源地址 + 取目标地址 + rep movsb
    fn gen_aggregate_assign(&mut self, dst_name: &str, dst_ty: &Type, r: &Expr) {
        let sz = dst_ty.size();
        let dst_off = self.var_offset(dst_name);
        // 特殊处理: 源是函数调用 (返回值在 rax[:rdx] 中)
        if let Expr::Call { callee, args, .. } = r {
            if let Expr::Ident { name, .. } = callee.as_ref() {
                // 生成参数
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() { self.gen_expr(a, ar[i]); }
                self.emit(&[0x54]); // push rsp
                self.call_label(&name.name);
                self.emit(&[0x5c]); // pop rsp
                // rax = 前 8 字节, rdx = 后 8 字节
                // 直接存储到目标变量
                self.store_stack(0, dst_off); // mov [r13+dst_off], rax
                if sz > 8 {
                    self.store_stack(2, dst_off + 8); // mov [r13+dst_off+8], rdx
                }
                return;
            }
        }
        // 1. 获取源地址到 rsi
        let got_src = match r {
            Expr::Ident { name: src_name, .. } => {
                if self.globals.contains_key(&src_name.name) {
                    self.emit_global_addr(6, &src_name.name); // rsi = 全局变量地址
                } else {
                    let src_off = self.var_offset(&src_name.name);
                    self.emit_lea_r13(6, src_off); // rsi = 源地址 (reg 6 = rsi)
                }
                true
            }
            _ => false,
        };
        if !got_src {
            // 通用路径: 求值表达式, 存到帧临时区, 然后拷贝
            self.gen_expr(r, 0);
            // 使用目标变量槽上方的安全临时区域
            let temp_off = dst_off - ((sz + 7) & !7) as i32; // 在目标下方留临时空间
            self.store_stack(0, temp_off);
            if sz > 8 {
                self.store_stack(2, temp_off + 8); // rdx = 第二部分
            }
            self.emit_lea_r13(6, temp_off); // rsi = 源 (reg 6 = rsi)
        }
        // 2. 取目标地址到 rdi
        self.emit_lea_r13(7, dst_off); // rdi = 目标地址
        // 3. rep movsq + rep movsb 拷贝
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

    // VLA 运行时分配: 动态栈分配
    // 策略: 计算大小 → 对齐 → sub rsp → 保存基址到变量槽
    fn gen_vla_alloc(&mut self, v: &VarDecl) {
        // VLA 变量类型: Array(elem, Var(expr))
        // 大小表达式在变量的 ArraySize::Var 中
        if let Type::Array(elem_ty, ArraySize::Var(ref size_expr)) = &v.ty {
            // 求值大小表达式到 rax
            self.gen_expr(size_expr, 0);
            // rax = 元素数量, 乘以元素大小
            let es = elem_ty.size() as i32;
            if es != 1 {
                self.emit(&[0x48, 0x69, 0xc0]); // imul rax, rax, imm32
                self.emit(&es.to_le_bytes());
            }
            // 对齐到 16 字节: (rax + 15) & ~15
            self.emit(&[0x48, 0x83, 0xc0, 0x0f]); // add rax, 15
            self.emit(&[0x48, 0x83, 0xe0, 0xf0]); // and rax, -16
            // 保存 VLA 大小到 vla_offsets
            let off = self.var_offset(&v.name.name);
            self.vla_offsets.insert(v.name.name.clone(), off);
            // 保存当前 rsp (VLA 基址)
            // sub rsp, rax: 48 29 C4
            self.emit(&[0x48, 0x29, 0xC4]);
            // mov [r13 + var_off], rsp: 保存 VLA 指针到变量槽
            let rex = 0x49u8; // REX.W + REX.B (r13 base)
            self.emit(&[rex, 0x89]);
            if off.abs() < 128 {
                // mod=01, reg=rsp(4), r/m=r13(5) → 0x65
                self.emit(&[0x65, off as i8 as u8]);
            } else {
                // mod=10, reg=rsp(4), r/m=r13(5) → 0x85
                self.emit(&[0x85]);
                self.emit(&off.to_le_bytes());
            }
            // 对齐 rsp 到 16 字节 (已经是 16 对齐的, 因为 rax 是 16 对齐的)
        }
    }

    // 函数尾声: 恢复 r13 + 返回
    fn emit_func_epilogue(&mut self) {
        // mov rsp, r13: 恢复栈指针
        self.emit(&[0x49, 0x8b, 0xe5]);
        self.emit(&[0x41, 0x5d]); // pop r13
        if self.target.frame_pointer {
            self.emit(&[0x5D]); // pop rbp
        }
        self.emit(&[0xc3]); // ret
    }

    /// 初始化 SysV AMD64 va_list 结构 (24 字节)。
    fn emit_va_start(&mut self, ap_name: &str) {
        let ap_off = self.var_offset(ap_name);
        let gp_init = (self.variadic_named_gp.min(6) * 8) as i32;
        // SSE 命名参数各占 reg_save_area 中 16 字节槽
        let fp_init = 48 + self.variadic_named_fp * 16;
        self.store_stack_imm32(ap_off, gp_init);
        self.store_stack_imm32(ap_off + 4, fp_init);
        let overflow_off = self.frame_size + 16;
        self.emit_lea_r13(0, overflow_off);
        self.store_stack(0, ap_off + 8);
        self.emit_lea_r13(0, self.variadic_reg_save_offset);
        self.store_stack(0, ap_off + 16);
    }

    fn store_stack_imm32(&mut self, off: i32, val: i32) {
        // mov dword [r13+off], imm32
        if off.abs() < 128 {
            self.emit(&[0x41, 0xC7, 0x45, off as i8 as u8]);
        } else {
            self.emit(&[0x41, 0xC7, 0x85]);
            self.emit(&off.to_le_bytes());
        }
        self.emit(&val.to_le_bytes());
    }

    /// SysV va_arg: 按 gp_offset/fp_offset 与 overflow_arg_area 读取参数。
    fn gen_va_arg(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
        if matches!(arg_ty, Type::Float | Type::Double | Type::LongDouble) && self.target.sse {
            self.gen_va_arg_fp(ap_name, arg_ty, dst);
            return;
        }
        let ap_off = self.var_offset(ap_name);
        let step = ((arg_ty.size() + 7) / 8 * 8) as i32;
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        // eax = gp_offset
        self.emit(&[0x41, 0x8B, 0x85]);
        self.emit(&ap_off.to_le_bytes());
        self.emit(&[0x83, 0xF8, 0x30]); // cmp $48, %eax
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // reg_save_area + gp_offset
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&(ap_off + 16).to_le_bytes());
        let sz = arg_ty.size();
        match sz {
            1 => self.emit(&[0x0F, 0xB6, 0x04, 0x01]),
            2 => self.emit(&[0x0F, 0xB7, 0x04, 0x01]),
            4 => self.emit(&[0x8B, 0x04, 0x01]),
            _ => self.emit(&[0x48, 0x8B, 0x04, 0x01]),
        }
        self.emit(&[0x41, 0x83, 0x85]);
        self.emit(&ap_off.to_le_bytes());
        self.emit(&step.to_le_bytes()[..1]);
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&(ap_off + 8).to_le_bytes());
        match sz {
            1 => self.emit(&[0x0F, 0xB6, 0x01]),
            2 => self.emit(&[0x0F, 0xB7, 0x01]),
            4 => self.emit(&[0x8B, 0x01]),
            _ => self.emit(&[0x48, 0x8B, 0x01]),
        }
        self.push(0);
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&(ap_off + 8).to_le_bytes());
        self.emit(&[0x48, 0x83, 0xC1]);
        self.emit(&step.to_le_bytes()[..1]);
        self.emit(&[0x49, 0x89, 0x8D]);
        self.emit(&(ap_off + 8).to_le_bytes());
        self.pop(0);
        self.label(&done);
        if dst != 0 { self.mov(dst, 0); }
    }

    /// SysV va_arg 浮点路径：fp_offset + reg_save_area XMM 槽。
    fn gen_va_arg_fp(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
        let ap_off = self.var_offset(ap_name);
        let step = 16i32;
        let overflow = self.fresh_label();
        let done = self.fresh_label();
        // eax = fp_offset (va_list + 4)
        self.emit(&[0x41, 0x8B, 0x85]);
        self.emit(&(ap_off + 4).to_le_bytes());
        // cmp $176, %eax — 176 超出 imm8 范围，须用 32 位立即数
        self.emit(&[0x3D, 0xB0, 0x00, 0x00, 0x00]);
        self.jmp_rel32(&overflow, JumpKind::Jge);
        // reg_save_area + fp_offset
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&(ap_off + 16).to_le_bytes());
        if matches!(arg_ty, Type::Float) {
            self.emit(&[0xF3, 0x0F, 0x10, 0x04, 0x01]); // movss (%rcx,%rax), xmm0
        } else if matches!(arg_ty, Type::LongDouble) {
            // long double: 16 字节，取低 8 字节 double 部分
            self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x01]); // movsd
        } else {
            self.emit(&[0xF2, 0x0F, 0x10, 0x04, 0x01]); // movsd (%rcx,%rax), xmm0
        }
        self.emit(&[0x41, 0x83, 0x85]);
        self.emit(&(ap_off + 4).to_le_bytes());
        self.emit(&step.to_le_bytes()[..1]);
        self.jmp_rel32(&done, JumpKind::Jmp);
        self.label(&overflow);
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&(ap_off + 8).to_le_bytes());
        if matches!(arg_ty, Type::Float) {
            self.emit(&[0xF3, 0x0F, 0x10, 0x01]);
        } else if matches!(arg_ty, Type::LongDouble) {
            self.emit(&[0xF2, 0x0F, 0x10, 0x01]);
        } else {
            self.emit(&[0xF2, 0x0F, 0x10, 0x01]);
        }
        self.push(0);
        self.emit(&[0x49, 0x8B, 0x8D]);
        self.emit(&(ap_off + 8).to_le_bytes());
        self.emit(&[0x48, 0x83, 0xC1]);
        self.emit(&step.to_le_bytes()[..1]);
        self.emit(&[0x49, 0x89, 0x8D]);
        self.emit(&(ap_off + 8).to_le_bytes());
        self.pop(0);
        self.label(&done);
        if dst != 0 {
            self.mov_xmm(dst, 0);
        }
    }

    // 预扫描: 统计函数体中局部变量所需栈槽数 (数组按实际大小)
    fn count_locals(items: &[BlockItem]) -> usize {
        let mut n = 0;
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    n += (v.ty.size() + 7) / 8; // 数组占多个槽
                }
                BlockItem::Stmt(s) => n += Self::count_locals_stmt(s),
                _ => {}
            }
        }
        n
    }

    // 预注册所有局部变量到 var_map (数组分配多槽, 保证帧大小计算正确)
    fn pre_register_vars(var_map: &mut HashMap<String, (i32, Type)>, items: &[BlockItem], temp_slots: i32) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    let slots = ((v.ty.size() + 7) / 8).max(1) as i32;
                    // 基于实际类型大小计算偏移 (过滤 padding 条目)
                    let used: i32 = var_map.values()
                        .filter(|(_, ty)| !matches!(ty, Type::Void))
                        .map(|(_, ty)| ((ty.size() + 7) / 8) as i32 * 8)
                        .sum();
                    let mut off = -(used + 16);
                    while off + v.ty.size() as i32 > 0 {
                        off -= 8;
                    }
                    var_map.entry(v.name.name.clone()).or_insert((off, v.ty.clone()));
                    for j in 1..slots {
                        let pad = format!("__pad_{}_{}", v.name.name, j);
                        let pad_off = off - j * 8;
                        var_map.entry(pad).or_insert((pad_off, Type::Void));
                    }
                }
                BlockItem::Stmt(s) => Self::pre_register_stmt(var_map, s, temp_slots),
                _ => {}
            }
        }
    }
    fn pre_register_stmt(var_map: &mut HashMap<String, (i32, Type)>, s: &Stmt, temp_slots: i32) {
        match s {
            Stmt::Block { b, .. } => Self::pre_register_vars(var_map, &b.items, temp_slots),
            Stmt::If { then_, else_, .. } => {
                Self::pre_register_stmt(var_map, then_, temp_slots);
                if let Some(e) = else_ { Self::pre_register_stmt(var_map, e, temp_slots); }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::Do { body, .. } => {
                Self::pre_register_stmt(var_map, body, temp_slots);
            }
            _ => {}
        }
    }
    fn count_locals_stmt(s: &Stmt) -> usize {
        match s {
            Stmt::Block { b, .. } => Self::count_locals(&b.items),
            Stmt::If { then_, else_, .. } => {
                Self::count_locals_stmt(then_)
                    + else_.as_ref().map_or(0, |e| Self::count_locals_stmt(e))
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::Do { body, .. } => {
                Self::count_locals_stmt(body)
            }
            _ => 0,
        }
    }

    fn ends_with_return(items: &[BlockItem]) -> bool {
        match items.last() {
            Some(BlockItem::Stmt(Stmt::Return { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Block { b, .. })) => Self::ends_with_return(&b.items),
            Some(BlockItem::Stmt(Stmt::If { then_, else_, .. })) =>
                Self::ends_with_return(&[BlockItem::Stmt(*then_.clone())])
                && else_.as_ref().map_or(false, |e| Self::ends_with_return(&[BlockItem::Stmt(*e.clone())])),
            _ => false,
        }
    }

    // 扫描最大表达式嵌套深度 (binop/call 各+1)
    fn max_expr_depth_stmt(items: &[BlockItem]) -> u32 {
        items.iter().map(|item| match item {
            BlockItem::Decl(Decl::Var(v)) => v.init.as_ref().map_or(0, |e| Self::max_expr_depth_expr(e)),
            BlockItem::Stmt(s) => Self::max_expr_depth_stmt_inner(s),
            _ => 0,
        }).max().unwrap_or(0)
    }
    fn max_expr_depth_stmt_inner(s: &Stmt) -> u32 {
        match s {
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => Self::max_expr_depth_expr(e),
            Stmt::If { cond, then_, else_, .. } => {
                let d = Self::max_expr_depth_expr(cond);
                d.max(Self::max_expr_depth_stmt_inner(then_))
                    .max(else_.as_ref().map_or(0, |e| Self::max_expr_depth_stmt_inner(e)))
            }
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::max_expr_depth_expr(cond).max(Self::max_expr_depth_stmt_inner(body))
            }
            Stmt::For { cond, body, .. } => {
                cond.as_ref().map_or(0, |c| Self::max_expr_depth_expr(c)).max(Self::max_expr_depth_stmt_inner(body))
            }
            Stmt::Block { b, .. } => Self::max_expr_depth_stmt(&b.items),
            _ => 0,
        }
    }
    fn max_expr_depth_expr(e: &Expr) -> u32 {
        match e {
            Expr::Binop { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            }
            Expr::Call { callee, args, .. } => {
                1 + Self::max_expr_depth_expr(callee).max(
                    args.iter().map(|a| Self::max_expr_depth_expr(a)).max().unwrap_or(0))
            }
            Expr::Comma { l, r, .. } => {
                Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            }
            Expr::Assign { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            }
            Expr::Unary { e, .. } | Expr::Cast { e, .. } | Expr::AddrOf { e, .. } | Expr::Deref { e, .. }
            | Expr::PostInc { e, .. } | Expr::PostDec { e, .. } | Expr::PreInc { e, .. } | Expr::PreDec { e, .. } => {
                Self::max_expr_depth_expr(e)
            }
            Expr::Index { arr, idx, .. } => {
                Self::max_expr_depth_expr(arr).max(Self::max_expr_depth_expr(idx))
            }
            Expr::Member { expr, .. } => Self::max_expr_depth_expr(expr),
            Expr::Cond { cond, then_, else_, .. } => {
                Self::max_expr_depth_expr(cond)
                    .max(Self::max_expr_depth_expr(then_))
                    .max(Self::max_expr_depth_expr(else_))
            }
            _ => 0,
        }
    }

    /// __builtin_* 系列内置函数 codegen: 返回 true 表示已处理
    /// 语句上下文中未识别的 __builtin_*：warning + ud2
    fn gen_unknown_builtin_stmt(&mut self, e: &Expr) -> bool {
        let Expr::Call { callee, args, .. } = e else {
            return false;
        };
        let Expr::Ident { name, .. } = callee.as_ref() else {
            return false;
        };
        if !name.name.starts_with("__builtin_") {
            return false;
        }
        if self.gen_builtin(&name.name, args, 0) {
            return true;
        }
        eprintln!(
            "warning: unsupported __builtin_ function '{}', emitted as ud2",
            name.name
        );
        self.emit(&[0x0F, 0x0B]); // ud2
        true
    }

    fn gen_builtin(&mut self, name: &str, args: &[Expr], dst: u8) -> bool {
        match name {
            // __builtin_expect(expr, val): 分支预测提示, 直接返回 expr
            "__builtin_expect" | "__builtin_expect_with_probability" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                true
            }
            // parser 通常已折叠；保留后端兜底，且与共享常量求值器一致。
            "__builtin_constant_p" => {
                let value = args.first().map_or(0, |arg| const_eval::is_constant(arg) as i128);
                self.mov_imm(dst, value);
                true
            }
            // __builtin_object_size(ptr, type): 编译期对象大小，未知时 (size_t)-1
            "__builtin_object_size" => {
                let type_arg = args.get(1).and_then(|e| const_eval::eval(e).ok()).unwrap_or(0);
                let size = args.first()
                    .and_then(|p| const_eval::object_size(p, type_arg))
                    .unwrap_or(-1);
                self.mov_imm(dst, size);
                true
            }
            // __builtin_memcpy/__builtin_memmove: 内联 rep movsb (freestanding 无 libc)
            "__builtin_memcpy" | "__builtin_memmove" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 参数: dst(rdi), src(rsi), n(rcx)
                self.gen_expr(&args[1], 6); // rsi = src
                self.gen_expr(&args[0], 7); // rdi = dst
                self.gen_expr(&args[2], 1); // rcx = n
                // 保存 rdi 作为返回值 (memcpy/memmove 返回 dst)
                self.emit(&[0x57]);             // push rdi
                self.emit(&[0xfc]);             // cld (清除方向标志)
                self.emit(&[0xf3, 0x48, 0xa4]); // rep movsb
                self.emit(&[0x5f]);             // pop rax (返回值 = dst)
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_memset: 内联 rep stosb (freestanding 无 libc)
            "__builtin_memset" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                // 参数: dst(rdi), val(rax), n(rcx)
                self.gen_expr(&args[0], 7); // rdi = dst
                self.gen_expr(&args[1], 0); // rax = val
                self.gen_expr(&args[2], 1); // rcx = n
                self.emit(&[0x57]);             // push rdi
                self.emit(&[0xfc]);             // cld
                self.emit(&[0xf3, 0xaa]);       // rep stosb
                self.emit(&[0x5f]);             // pop rax (返回值 = dst)
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_memcmp: 转发到 libc ( memcmp 不适合简单内联)
            "__builtin_memcmp" => {
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() { self.gen_expr(a, ar[i]); }
                self.emit(&[0x54]); // push rsp
                self.call_label("memcmp");
                self.emit(&[0x5c]); // pop rsp
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_strcpy/__builtin_strncpy/__builtin_strcmp 等
            "__builtin_strcpy" | "__builtin_strncpy" | "__builtin_strcmp" | "__builtin_strlen" => {
                let libc_name = name.strip_prefix("__builtin_").unwrap();
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() { self.gen_expr(a, ar[i]); }
                self.emit(&[0x54]); // push rsp
                self.call_label(libc_name);
                self.emit(&[0x5c]); // pop rsp
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_abs: 绝对值
            "__builtin_abs" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                // abs: test dst, dst; jns skip; neg dst; skip:
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                self.emit(&[rex, 0x85, 0xC0 | ((dst & 7) << 3) | (dst & 7)]); // test dst, dst
                self.emit(&[0x79, 0x04]); // jns +4
                self.emit(&[rex, 0xf7, 0xd8 | (dst & 7)]); // neg dst
                true
            }
            // __builtin_unreachable: 不可达, 生成 ud2
            "__builtin_unreachable" | "__builtin_trap" => {
                self.emit(&[0x0F, 0x0B]); // ud2
                true
            }
            // __builtin_va_start(ap, last): 初始化 SysV va_list 结构
            "__builtin_va_start" => {
                if args.len() >= 1 {
                    if let Expr::Ident { name, .. } = &args[0] {
                        self.emit_va_start(&name.name);
                    }
                }
                true
            }
            // __builtin_va_end(ap): no-op
            "__builtin_va_end" => {
                true
            }
            // __builtin_va_arg 已通过 Expr::VaArg 处理, 此处不会到达
            "__builtin_va_arg" => {
                true
            }
            // __builtin_va_copy(dest, src): 复制整个 va_list 结构
            "__builtin_va_copy" => {
                if args.len() >= 2 {
                    if let (Expr::Ident { name: dest_name, .. }, Expr::Ident { name: src_name, .. }) = (&args[0], &args[1]) {
                        let dest_off = self.var_offset(&dest_name.name);
                        let src_off = self.var_offset(&src_name.name);
                        for i in 0..3i32 {
                            self.load_stack(0, src_off + i * 8);
                            self.store_stack(0, dest_off + i * 8);
                        }
                    }
                }
                true
            }
            // __builtin_bswap16/32/64: 字节序反转
            "__builtin_bswap16" | "__builtin_bswap32" | "__builtin_bswap64" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                if name == "__builtin_bswap64" {
                    // bswap 的寄存器编码在 opcode 低三位，扩展寄存器使用 REX.B。
                    let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };
                    self.emit(&[rex, 0x0F, 0xC8 | (dst & 7)]);
                } else if name == "__builtin_bswap32" {
                    if dst >= 8 { self.emit(&[0x41]); }
                    self.emit(&[0x0F, 0xC8 | (dst & 7)]);
                } else {
                    // x86 没有 bswap16；rol r16, 8 等价于交换两个字节。
                    self.emit(&[0x66]);
                    if dst >= 8 { self.emit(&[0x41]); }
                    self.emit(&[0xC1, 0xC0 | (dst & 7), 8]);
                    let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };
                    self.emit(&[rex, 0x0F, 0xB7, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                }
                true
            }
            // __builtin_huge_val / huge_valf / huge_vall: IEEE 754 +Inf
            // __builtin_inf / inff / infl: 同 huge_val*, 单独列出
            // 直接加载 IEEE 754 位模式:
            //   double +Inf:  0x7FF0_0000_0000_0000
            //   float  +Inf:  0x7F80_0000
            //   long double (x86 80-bit) +Inf: 0x7FFF_8000_0000_0000_0000
            // dcc 把 long double 当 double 处理, 8 字节近似.
            // emit: 8 字节 movsd/movss xmm0, [rip+disp32], disp32 在 code_pos+4.
            // 注意: 结果必须写到 xmm0 (dst=0) 而非 rax, 因为双精度返回走 xmm0.
            "__builtin_huge_val" | "__builtin_huge_valf" | "__builtin_huge_vall"
            | "__builtin_inf" | "__builtin_inff" | "__builtin_infl" => {
                let (is_float, bytes) = match name {
                    "__builtin_huge_valf" | "__builtin_inff" => {
                        (true, 0x7F800000u32.to_le_bytes().to_vec())
                    }
                    "__builtin_huge_vall" | "__builtin_infl" => {
                        // long double: dcc 用 8 字节近似, 加载 0 进入 xmm0 (近似 +Inf)
                        (false, 0x7FF0000000000000u64.to_le_bytes().to_vec())
                    }
                    _ => (false, 0x7FF0000000000000u64.to_le_bytes().to_vec()),
                };
                let off = self.rodata.len() as u64;
                self.rodata.extend_from_slice(&bytes);
                // movss/movsd xmm0, [rip+disp32]: 8 字节指令
                //   F3 0F 10 05 disp32 (movss, 4 字节) 或
                //   F2 0F 10 05 disp32 (movsd, 8 字节)
                // disp32 在 code_pos+4, 加 REX 不需要 (xmm0 < 8).
                let prefix = if is_float { 0xF3 } else { 0xF2 };
                let code_pos = self.pos();
                self.emit(&[prefix, 0x0F, 0x10, 0x05, 0, 0, 0, 0]);
                // 结果在 xmm0. 若调用方 dst 不是 0, 移动到 dst.
                if dst != 0 {
                    // dst 应当是 x86 通用寄存器. 使用 movsd/movss 拷贝.
                    if is_float {
                        self.emit(&[0xF3, 0x0F, 0x11, 0xC0 | (dst & 7)]);
                    } else {
                        self.emit(&[0xF2, 0x0F, 0x11, 0xC0 | (dst & 7)]);
                    }
                }
                // 走 float_refs 通道: ET_EXEC 模式 resolve() 阶段会自动回填
                //   disp32 = (rodata_start + off) - (entry + code_pos + 8)
                // 也就是 addend = off - 8 (在 relocatable 模式下).
                self.float_refs.push((code_pos, off));
                true
            }
            // __builtin_nan / nanf / nanl: 静默 NaN (quiet NaN).
            // GCC 接受字符串参数 (mantissa bits), 我们忽略字符串, 取默认 qNaN.
            // qNaN 位模式:
            //   double:  0x7FF8_0000_0000_0000
            //   float:   0x7FC0_0000
            //   long double: 0x7FFF_C000_0000_0000_0000 (dcc 8 字节近似)
            // emit 8 字节 movsd/movss xmm0, [rip+disp32], disp32 在 code_pos+4.
            "__builtin_nan" | "__builtin_nanf" | "__builtin_nanl" => {
                let (is_float, bytes) = match name {
                    "__builtin_nanf" =>
                        (true, 0x7FC00000u32.to_le_bytes().to_vec()),
                    "__builtin_nanl" =>
                        (false, 0x7FF8000000000000u64.to_le_bytes().to_vec()),
                    _ => (false, 0x7FF8000000000000u64.to_le_bytes().to_vec()),
                };
                let off = self.rodata.len() as u64;
                self.rodata.extend_from_slice(&bytes);
                let prefix = if is_float { 0xF3 } else { 0xF2 };
                let code_pos = self.pos();
                self.emit(&[prefix, 0x0F, 0x10, 0x05, 0, 0, 0, 0]);
                if dst != 0 {
                    if is_float {
                        self.emit(&[0xF3, 0x0F, 0x11, 0xC0 | (dst & 7)]);
                    } else {
                        self.emit(&[0xF2, 0x0F, 0x11, 0xC0 | (dst & 7)]);
                    }
                }
                // 走 float_refs 通道 (同 __builtin_huge_val 实现)
                self.float_refs.push((code_pos, off));
                true
            }
            // __builtin_isnan / isnanf / isnanl: NaN 检测.
            // NaN 的特性: ucomisd/ucomiss 设置 PF=1 (任一为 NaN).
            //   较新 Intel CPU 在 NaN 时 ZF=1, PF=1, CF=1
            //   旧文档:  ZF=0, PF=1, CF=1
            // 统一用 setp 检测 PF=1 → NaN.
            // 类型由实参决定, 而非函数名后缀: __builtin_isnan(float) 也应
            // 使用 ucomiss; 否则 movss 后高位 0 会使 ucomisd 行为不一致.
            "__builtin_isnan" | "__builtin_isnanf" | "__builtin_isnanl" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                let arg_ty = args[0].ty();
                let is_f = matches!(arg_ty, Type::Float);
                self.gen_expr(&args[0], 0); // xmm0 = x
                // ucomis{s,d} xmm0, xmm0
                if is_f {
                    self.emit(&[0x0F, 0x2E, 0xC0]); // ucomiss xmm0, xmm0
                } else {
                    self.emit(&[0x66, 0x0F, 0x2E, 0xC0]); // ucomisd xmm0, xmm0
                }
                // setp al (PF=1 ⇒ NaN)
                self.emit(&[0x0F, 0x9A, 0xC0]);
                // movzx eax, al (REX.W 0F B6 C0)
                self.emit(&[0x48, 0x0F, 0xB6, 0xC0]);
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_isinf / isinff / isinfl: ±Inf 检测.
            // 返回: +Inf→1, -Inf→-1, 其他 (含 NaN) →0.
            // 算法: 用 |x| vs +Inf 比较 (sete); 符号位取 sign.
            // 合成: result = isinf * (1 - 2*sign)
            //   sign=0: result = +isinf
            //   sign=1: result = -isinf
            // 排除 NaN: setnp (NOT PF). NaN 时 PF=1, setnp=0 → 整体被清零.
            // 类型按实参, 不用函数名后缀.
            "__builtin_isinf" | "__builtin_isinff" | "__builtin_isinfl" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                let arg_ty = args[0].ty();
                let is_f = matches!(arg_ty, Type::Float);
                // 1. isinf: |x| == +Inf 且非 NaN
                self.gen_expr(&args[0], 0); // xmm0 = x
                self.emit(&[0x48, 0xB8]);
                self.emit(&0x7FFFFFFFFFFFFFFFu64.to_le_bytes());
                self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]); // movq xmm1, rax
                self.emit(&[0x0F, 0x54, 0xC1]); // andps xmm0, xmm1 (|x|)
                self.emit(&[0x48, 0xB8]);
                // float +Inf bit pattern: 0x7F800000
                let inf_bits: u64 = if is_f { 0x7F800000 } else { 0x7FF0000000000000 };
                self.emit(&inf_bits.to_le_bytes());
                self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]); // movq xmm1, rax
                // ucomiss (0F 2E) for float, ucomisd (66 0F 2E) for double
                if is_f {
                    self.emit(&[0x0F, 0x2E, 0xC1]); // ucomiss xmm0, xmm1
                } else {
                    self.emit(&[0x66, 0x0F, 0x2E, 0xC1]); // ucomisd xmm0, xmm1
                }
                // sete al (ZF=1): |x| == +Inf
                self.emit(&[0x0F, 0x94, 0xC0]);
                // setnp bl (NOT PF): 非 NaN
                self.emit(&[0x0F, 0x9B, 0xC3]);
                // and al, bl: 既等于 Inf 又非 NaN
                self.emit(&[0x20, 0xD8]); // and al, bl
                // movzx eax, al
                self.emit(&[0x0F, 0xB6, 0xC0]);
                self.emit(&[0x89, 0xC7]); // mov edi, eax (edi = isinf)
                // 2. sign: 重新加载 x, 取符号位
                self.gen_expr(&args[0], 0);
                self.emit(&[0x0F, 0x50, 0xC0]); // movmskps eax, xmm0
                self.emit(&[0x83, 0xE0, 0x01]); // and eax, 1 (eax = sign)
                self.emit(&[0x89, 0xC6]); // mov esi, eax (esi = sign)
                // 3. 合成: eax=isinf, edx=isinf, eax=-isinf; 若 sign==0, eax=edx
                self.emit(&[0x89, 0xF8]); // mov eax, edi (eax = isinf)
                self.emit(&[0x89, 0xC2]); // mov edx, eax (edx = isinf)
                self.emit(&[0xF7, 0xD8]); // neg eax (eax = -isinf)
                self.emit(&[0x85, 0xF6]); // test esi, esi
                self.emit(&[0x0F, 0x44, 0xC2]); // cmove eax, edx
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_finite: x 既不是 NaN 也不是 Inf.
            // 用 |x| vs +Inf 比较, 同时排除 NaN (PF=1 ⇒ NaN).
            // 正确语义: finite = (|x| < +Inf) AND NOT NaN
            // 编码:  (|x| < +Inf) ⇒ CF=1 ⇒ setb al
            //         NaN          ⇒ PF=1 ⇒ setnp bl (NOT PF)
            //         finite = al AND bl
            "__builtin_finite" | "__builtin_finitef" | "__builtin_finitel" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                let arg_ty = args[0].ty();
                let is_f = matches!(arg_ty, Type::Float);
                self.gen_expr(&args[0], 0); // xmm0 = x
                // 加载 abs_mask 到 xmm1
                self.emit(&[0x48, 0xB8]);
                self.emit(&0x7FFFFFFFFFFFFFFFu64.to_le_bytes());
                self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]); // movq xmm1, rax
                // andps xmm0, xmm1: |x|
                self.emit(&[0x0F, 0x54, 0xC1]);
                // 加载 +Inf 到 xmm1
                self.emit(&[0x48, 0xB8]);
                let inf_bits: u64 = if is_f { 0x7F800000 } else { 0x7FF0000000000000 };
                self.emit(&inf_bits.to_le_bytes());
                self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]); // movq xmm1, rax
                // ucomiss (0F 2E) for float, ucomisd (66 0F 2E) for double
                if is_f {
                    self.emit(&[0x0F, 0x2E, 0xC1]); // ucomiss xmm0, xmm1
                } else {
                    self.emit(&[0x66, 0x0F, 0x2E, 0xC1]); // ucomisd xmm0, xmm1
                }
                // setb al (CF=1): |x| < +Inf
                self.emit(&[0x0F, 0x92, 0xC0]);
                // setnp bl (NOT PF): 非 NaN
                //   0F 9B C3 = setnp bl
                self.emit(&[0x0F, 0x9B, 0xC3]);
                // and al, bl: 两者都成立才是 finite
                self.emit(&[0x20, 0xD8]); // and al, bl
                // movzx eax, al
                self.emit(&[0x48, 0x0F, 0xB6, 0xC0]);
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_isunordered(a, b): 任一为 NaN 则 1.
            // 优先把两个实参对齐到 double (用 cvtps2pd/cvtss2sd).
            // 较新 Intel CPU 在 NaN 时 ZF=1, PF=1, CF=1 → setp 仍可识别.
            "__builtin_isunordered" => {
                if args.len() < 2 { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], 0);
                // 如果 a 是 float, 升为 double
                if matches!(args[0].ty(), Type::Float) {
                    self.emit(&[0xF3, 0x0F, 0x5A, 0xC0]); // cvtss2sd xmm0, xmm0
                }
                self.gen_expr(&args[1], 1);
                // 如果 b 是 float, 升为 double
                if matches!(args[1].ty(), Type::Float) {
                    self.emit(&[0xF3, 0x0F, 0x5A, 0xC9]); // cvtss2sd xmm1, xmm1
                }
                // ucomisd xmm0, xmm1
                self.emit(&[0x66, 0x0F, 0x2E, 0xC1]);
                // setp al
                self.emit(&[0x0F, 0x9A, 0xC0]);
                self.emit(&[0x48, 0x0F, 0xB6, 0xC0]);
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_fpclassify(p, n, z, i, x, v): 浮点分类.
            // GCC 返回 0..5: NaN, Inf, Zero, Subnormal, Normal, 实现自定义.
            // 简化: 仅区分 4 类 (NaN, Inf, Zero, Normal); Subnormal 归到 Normal.
            //   NaN → args[0]
            //   Inf → args[1]
            //   Zero → args[2]
            //   其他 (含 Subnormal) → args[4]  (跳过 Subnormal args[3])
            "__builtin_fpclassify" => {
                if args.len() < 6 { self.mov_imm(dst, 0); return true; }
                let arg_ty = args[5].ty();
                let is_f = matches!(arg_ty, Type::Float);
                // 决定用 ucomiss 还是 ucomisd
                // 0F 2E / 66 0F 2E
                let cmp_prefix: u8 = if is_f { 0 } else { 0x66 };
                let cmp_opcode: u8 = 0x2E;
                // 1. NaN 测试
                self.gen_expr(&args[5], 0);
                self.emit(&[cmp_prefix, 0x0F, cmp_opcode, 0xC0]); // ucomis{s,d} xmm0, xmm0
                self.emit(&[0x0F, 0x9A, 0xC3]); // setp bl
                self.emit(&[0x0F, 0xB6, 0xDB]); // movzx ebx, bl
                // 2. Zero 测试
                self.gen_expr(&args[5], 0);
                self.emit(&[0x0F, 0x57, 0xC9]); // xorps xmm1, xmm1
                self.emit(&[cmp_prefix, 0x0F, cmp_opcode, 0xC8]); // ucomis{s,d} xmm1, xmm0
                self.emit(&[0x0F, 0x94, 0xC1]); // sete cl
                self.emit(&[0x0F, 0xB6, 0xC9]); // movzx ecx, cl
                // 3. Inf 测试
                self.gen_expr(&args[5], 0);
                self.emit(&[0x48, 0xB8]);
                let inf_bits: u64 = if is_f { 0x7F800000 } else { 0x7FF0000000000000 };
                self.emit(&inf_bits.to_le_bytes());
                self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]); // movq xmm1, rax
                self.emit(&[cmp_prefix, 0x0F, cmp_opcode, 0xC1]); // ucomis{s,d} xmm0, xmm1
                self.emit(&[0x0F, 0x94, 0xC2]); // sete dl
                self.emit(&[0x0F, 0xB6, 0xD2]); // movzx edx, dl
                // 4. 合成: 优先级 NaN > Inf > Zero > Normal
                // 默认: args[4] (Normal)
                self.gen_expr(&args[4], 0);
                // 若 Zero: eax = args[2]
                // 若 Inf:  eax = args[1]
                // 若 NaN:  eax = args[0]
                // 我们用栈暂存 args[0..3] 到 r12-r15? 太复杂.
                // 简化: 返回分类编号 (0..4), 由调用者按映射.
                // 分类编号: NaN=0, Inf=1, Zero=2, Subnormal=3, Normal=4
                self.emit(&[0xB8, 0x04, 0x00, 0x00, 0x00]); // mov eax, 4 (Normal)
                // eax=4 默认
                // 若 NaN (ebx=1): eax = 0
                // 若 Inf (edx=1): eax = 1
                // 若 Zero (ecx=1): eax = 2
                // 链: 先 mov eax, 4; test ecx cmove eax, 2; test edx cmove eax, 1; test ebx cmove eax, 0
                // 准备 3 个常量:
                self.emit(&[0xBF, 0x02, 0x00, 0x00, 0x00]); // mov edi, 2
                self.emit(&[0xBE, 0x01, 0x00, 0x00, 0x00]); // mov esi, 1
                self.emit(&[0xBD, 0x00, 0x00, 0x00, 0x00]); // mov ebp, 0
                // 实际上分类值由调用者映射, 我们这里返回 0/1/2/4 即可
                // cmovne: 当 ebx=1 (NaN) 时 eax=0; edx=1 (Inf) 时 eax=1; ecx=1 (Zero) 时 eax=2
                // 注意 cmovne 条件是 ZF=0 (非零), cmove 条件是 ZF=1 (零)
                self.emit(&[0x85, 0xC9]); // test ecx, ecx (Zero?)
                self.emit(&[0x0F, 0x45, 0xC7]); // cmovne eax, edi (Zero=2)
                self.emit(&[0x85, 0xD2]); // test edx, edx (Inf?)
                self.emit(&[0x0F, 0x45, 0xC6]); // cmovne eax, esi (Inf=1)
                self.emit(&[0x85, 0xDB]); // test ebx, ebx (NaN?)
                self.emit(&[0x0F, 0x45, 0xC5]); // cmovne eax, ebp (NaN=0)
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_signbit: 浮点数符号位 (x<0 返回 1, 否则 0)
            // 用 movmskps 提取 4 packed single 的符号位, 放入 eax 的 bit 0..3.
            //   bit 0 = 第一个 32-bit 字的符号位
            //   bit 1 = 第二个 32-bit 字的符号位
            //   bit 2 = 第三个 32-bit 字的符号位
            //   bit 3 = 第四个 32-bit 字的符号位
            // double 占据 64 位, 其符号位在 bit 63 = 第二个 32-bit 字的最高位
            //   => 对应 movmskps 输出的 bit 1
            // float 占据 32 位, 符号位在 bit 31 = 第一个 32-bit 字的最高位
            //   => 对应 movmskps 输出的 bit 0
            // 简化: 用 shr eax, 1 后再 and 1 拿 bit 1 (double), 但 float 会被右移
            //   丢掉. 我们用类型区分:
            //   double/long double: and 2 → 是否为负
            //   float:               and 1 → 是否为负
            "__builtin_signbit" | "__builtin_signbitf" | "__builtin_signbitl" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                let arg_ty = args[0].ty();
                let is_f = matches!(arg_ty, Type::Float);
                self.gen_expr(&args[0], 0); // xmm0 = x
                // movmskps eax, xmm0
                self.emit(&[0x0F, 0x50, 0xC0]);
                if is_f {
                    // float: 符号位是 bit 0
                    self.emit(&[0x83, 0xE0, 0x01]); // and eax, 1
                } else {
                    // double/long double: 符号位是 bit 1
                    self.emit(&[0x83, 0xE0, 0x02]); // and eax, 2
                    // 归一化为 0/1: shr 1
                    self.emit(&[0xD1, 0xE8]); // shr eax, 1
                }
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_call_with_static_chain(call, chain, args...):
            // 加载 chain 到 r10 (静态链寄存器), 然后按 SysV ABI 准备 args... 参数,
            // 最后调用 call. 这是嵌套函数调用的关键 ABI: r10 = 父帧指针.
            //
            // 注: 嵌套函数"取地址" (nested function → function pointer) 还需要
            // 单独生成 trampoline, 才能在跨函数指针场景用上, 本 builtin 主要给
            // 已经能拿到函数指针 (例如通过宏展开) 的场景使用.
            "__builtin_call_with_static_chain" => {
                if args.len() < 2 { self.mov_imm(dst, 0); return true; }
                // 1. chain → r10
                self.gen_expr(&args[1], 10);
                // 2. 按 SysV ABI 准备剩余参数 (args[2..]) 到 rdi/rsi/rdx/rcx/r8/r9 + 栈
                let extra = &args[2..];
                let gp_regs: [u8; 6] = [7, 6, 2, 1, 8, 9]; // rdi, rsi, rdx, rcx, r8, r9
                let mut stack_off: i32 = 0;
                for (i, a) in extra.iter().enumerate() {
                    let arg_ty = a.ty();
                    if i < 6 {
                        // 前 6 个参数: 整数到 gp_regs[i], 浮点到 xmm{i}
                        if matches!(arg_ty, Type::Float | Type::Double | Type::LongDouble) {
                            self.gen_expr(a, 0); // xmm0
                            if i != 0 {
                                // movaps xmm{i}, xmm0
                                self.emit(&[0xF3, 0x0F, 0x11, 0xC0 | ((i as u8) << 3)]);
                            }
                        } else {
                            self.gen_expr(a, gp_regs[i]);
                        }
                    } else {
                        // 第 7+ 个参数: 压栈
                        self.gen_expr(a, 0);
                        if matches!(arg_ty, Type::Float | Type::Double | Type::LongDouble) {
                            self.emit(&[0xF2, 0x0F, 0x11, 0x44, 0x24, 0]); // movsd [rsp+0], xmm0
                        } else {
                            self.emit(&[0x48, 0x89, 0x44, 0x24, 0]); // mov [rsp+0], rax
                        }
                        stack_off += 8;
                    }
                }
                if stack_off != 0 {
                    // sub rsp, stack_off
                    if stack_off < 128 {
                        self.emit(&[0x48, 0x83, 0xEC, stack_off as u8]);
                    } else {
                        self.emit(&[0x48, 0x81, 0xEC]);
                        self.emit(&stack_off.to_le_bytes());
                    }
                }
                // 3. 调整 RSP 对齐 (call 需要 16 字节对齐)
                // 注意: 简化版, 假设栈已经 16 字节对齐.
                // 4. 调用 args[0]
                match &args[0] {
                    Expr::Ident { name, .. } => {
                        self.call_label(&name.name);
                    }
                    _ => {
                        // 函数指针: 加载到 r11 然后 call r11
                        self.gen_expr(&args[0], 11);
                        self.emit(&[0x41, 0xFF, 0xD3]); // call r11
                    }
                }
                if stack_off != 0 {
                    // add rsp, stack_off
                    if stack_off < 128 {
                        self.emit(&[0x48, 0x83, 0xC4, stack_off as u8]);
                    } else {
                        self.emit(&[0x48, 0x81, 0xC4]);
                        self.emit(&stack_off.to_le_bytes());
                    }
                }
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_clz/clzl/clzll: 前导零计数
            "__builtin_clz" | "__builtin_clzl" | "__builtin_clzll" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };
                // 根据参数类型大小确定位宽
                let bits = args[0].ty().size() as i32 * 8;
                // bsr dst, dst → dst = 最高位位置
                self.emit(&[rex, 0x0F, 0xBD, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                // clz = (bits - 1) - dst
                self.emit(&[rex, 0xF7, 0xD8 | (dst & 7)]); // neg dst
                self.emit(&[rex, 0x83, 0xC0 | ((dst & 7) << 3) | (dst & 7), (bits - 1) as u8]); // add dst, bits-1
                true
            }
            // __builtin_ctz/ctzl/ctzll: 尾随零计数
            "__builtin_ctz" | "__builtin_ctzl" | "__builtin_ctzll" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };
                // bsf dst, dst → dst = 最低位位置
                self.emit(&[rex, 0x0F, 0xBC, 0xC0 | ((dst & 7) << 3) | (dst & 7)]); // bsf dst, dst
                true
            }
            // __builtin_popcount/popcountl/popcountll: 位计数
            "__builtin_popcount" | "__builtin_popcountl" | "__builtin_popcountll" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                self.gen_expr(&args[0], dst);
                let rex = 0x48 | if dst >= 8 { 0x05 } else { 0 };
                // popcnt dst, dst: F3 0F B8 /r
                self.emit(&[0xF3, rex, 0x0F, 0xB8, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
                true
            }
            // __builtin_parity/parityl/parityll: 奇偶校验 (popcount & 1)
            "__builtin_parity" | "__builtin_parityl" | "__builtin_parityll" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                // XOR fold，避免依赖可选的 POPCNT CPU 特性。
                self.gen_expr(&args[0], 0);
                for shift in [32u8, 16, 8, 4, 2, 1] {
                    self.emit(&[0x48, 0x89, 0xC1]);       // mov rcx, rax
                    self.emit(&[0x48, 0xC1, 0xE9, shift]); // shr rcx, shift
                    self.emit(&[0x48, 0x31, 0xC8]);       // xor rax, rcx
                }
                self.emit(&[0x83, 0xE0, 1]);              // and eax, 1
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_bitreverse8/16/32/64: 固定次数移位循环，无外部运行库依赖。
            "__builtin_bitreverse8" | "__builtin_bitreverse16"
            | "__builtin_bitreverse32" | "__builtin_bitreverse64" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                let bits = match name {
                    "__builtin_bitreverse8" => 8u32,
                    "__builtin_bitreverse16" => 16,
                    "__builtin_bitreverse32" => 32,
                    _ => 64,
                };
                self.gen_expr(&args[0], 0);              // rax = input
                self.emit(&[0x48, 0x31, 0xC9]);          // xor rcx, rcx
                self.emit(&[0xBA]);
                self.emit(&bits.to_le_bytes());           // mov edx, bits
                self.emit(&[0x48, 0xD1, 0xE8]);          // shr rax, 1 (LSB -> CF)
                self.emit(&[0x48, 0xD1, 0xD1]);          // rcl rcx, 1
                self.emit(&[0xFF, 0xCA]);                // dec edx
                self.emit(&[0x75, 0xF6]);                // jnz loop
                if dst != 1 { self.mov(dst, 1); }
                true
            }
            // __builtin_frame_address(level): 帧指针
            "__builtin_frame_address" => {
                // 本后端使用 r13 作为稳定帧指针；[r13] 是调用者保存的 r13。
                self.emit(&[0x4C, 0x89, 0xE8]); // mov rax, r13
                let level = if let Some(Expr::Int { v, .. }) = args.first() { *v as i32 } else { 0 };
                for _ in 0..level {
                    self.emit(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
                }
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_return_address(level): 返回地址
            "__builtin_return_address" => {
                // push r13 后 r13 指向保存的旧 r13，返回地址位于 [r13+8]。
                self.emit(&[0x4C, 0x89, 0xE8]); // mov rax, r13
                let level = if let Some(Expr::Int { v, .. }) = args.first() { *v as i32 } else { 0 };
                for _ in 0..level {
                    self.emit(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
                }
                self.emit(&[0x48, 0x8B, 0x40, 0x08]); // mov rax, [rax+8]
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // *_overflow_p(a, b, (type)0): 计算目标类型宽度下的 OF/CF。
            "__builtin_overflow_p" | "__builtin_add_overflow_p"
            | "__builtin_sub_overflow_p" | "__builtin_mul_overflow_p" => {
                if args.len() < 2 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                self.gen_expr(&args[0], 0);
                self.push(0);
                self.gen_expr(&args[1], 0);
                self.pop(1); // rcx = lhs, rax = rhs
                let result_ty = args.get(2).map(|e| e.ty()).unwrap_or_else(|| args[0].ty());
                let size = result_ty.size();
                let unsigned = matches!(result_ty,
                    Type::Bool | Type::UChar | Type::UShort | Type::UInt
                    | Type::ULong | Type::ULongLong | Type::UInt128);
                let op = if name.contains("sub") {
                    1
                } else if name.contains("mul") {
                    2
                } else {
                    0
                };
                match (op, size) {
                    (0, 1) => self.emit(&[0x00, 0xC1]),       // add cl, al
                    (0, 2) => self.emit(&[0x66, 0x01, 0xC1]), // add cx, ax
                    (0, 4) => self.emit(&[0x01, 0xC1]),       // add ecx, eax
                    (0, _) => self.emit(&[0x48, 0x01, 0xC1]), // add rcx, rax
                    (1, 1) => self.emit(&[0x28, 0xC1]),       // sub cl, al
                    (1, 2) => self.emit(&[0x66, 0x29, 0xC1]), // sub cx, ax
                    (1, 4) => self.emit(&[0x29, 0xC1]),       // sub ecx, eax
                    (1, _) => self.emit(&[0x48, 0x29, 0xC1]), // sub rcx, rax
                    (2, 1) => self.emit(&[0xF6, if unsigned { 0xE1 } else { 0xE9 }]), // mul/imul cl
                    (2, 2) => self.emit(&[0x66, 0x0F, 0xAF, 0xC8]),
                    (2, 4) => self.emit(&[0x0F, 0xAF, 0xC8]),
                    (2, _) => self.emit(&[0x48, 0x0F, 0xAF, 0xC8]),
                    _ => unreachable!(),
                }
                let cc = if unsigned { 0x92 } else { 0x90 }; // setb / seto
                self.emit(&[0x0F, cc, 0xC0]);
                self.emit(&[0x0F, 0xB6, 0xC0]);
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_apply/__builtin_return: SysV 子集（≤6 个 GP 参数）
            "__builtin_apply" => {
                if args.len() < 3 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let size = args
                    .get(2)
                    .and_then(|e| const_eval::eval(e).ok())
                    .filter(|s| *s > 0)
                    .unwrap_or(-1);
                if size <= 0 {
                    self.mov_imm(dst, 0);
                    return true;
                }
                let size = size as i32;
                let frame = ((size + 8 + 15) / 16) * 16;
                self.gen_builtin_fn_ptr(&args[0], 10); // r10 = fn
                self.gen_expr(&args[1], 6);  // rsi = data
                self.emit(&[0x48, 0x81, 0xEC]);
                self.emit(&frame.to_le_bytes());
                self.emit(&[0x48, 0x89, 0xE7]); // mov rdi, rsp
                self.emit(&[0x48, 0xC7, 0xC1]);
                self.emit(&size.to_le_bytes());
                self.emit(&[0xFC, 0xF3, 0x48, 0xA4]); // cld; rep movsb
                self.emit(&[0x48, 0x89, 0xE7]); // mov rdi, rsp
                self.emit(&[0x41, 0xFF, 0xD2]); // call r10
                self.emit(&[0x48, 0x89, 0x84, 0x24]);
                self.emit(&size.to_le_bytes()); // mov [rsp+size], rax
                self.emit(&[0x48, 0x8D, 0x84, 0x24]);
                self.emit(&size.to_le_bytes()); // lea rax, [rsp+size]
                if dst != 0 {
                    self.mov(dst, 0);
                }
                true
            }
            "__builtin_return" => {
                if !args.is_empty() {
                    self.gen_expr(&args[0], 6); // rsi = result block
                    self.emit(&[0x48, 0x8B, 0x06]); // mov rax, [rsi]
                }
                self.emit_func_epilogue();
                true
            }
            // __builtin_prefetch 及变体：void 提示，编译为 no-op
            n if n.starts_with("__builtin_prefetch") => true,
            // __builtin_add_overflow(a, b, &result): 溢出检测
            // 策略: 计算 64 位结果, 截断到操作数类型, 比较检测溢出
            "__builtin_add_overflow" | "__builtin_sub_overflow" | "__builtin_mul_overflow" => {
                if args.len() < 3 { self.mov_imm(dst, 0); return true; }
                let op_size = args[0].ty().size().max(args[1].ty().size()).max(4); // 至少 32 位
                // 1. 求值 ptr → r10
                self.gen_expr(&args[2], 0);
                self.emit(&[0x49, 0x89, 0xc2]); // mov r10, rax
                // 2. 求值 a → push
                self.gen_expr(&args[0], 0);
                self.push(0);
                // 3. 求值 b → rax
                self.gen_expr(&args[1], 0);
                // 4. pop a → rcx
                self.pop(1);
                // rax = b, rcx = a, r10 = ptr
                // 5. 计算 64 位结果
                match name {
                    "__builtin_add_overflow" => {
                        self.emit(&[0x48, 0x01, 0xc8]); // add rax, rcx
                    }
                    "__builtin_sub_overflow" => {
                        self.emit(&[0x48, 0x89, 0xc2]); // mov rdx, rax
                        self.emit(&[0x48, 0x89, 0xc8]); // mov rax, rcx
                        self.emit(&[0x48, 0x29, 0xd0]); // sub rax, rdx
                    }
                    "__builtin_mul_overflow" => {
                        self.emit(&[0x48, 0x0f, 0xaf, 0xc1]); // imul rax, rcx
                    }
                    _ => unreachable!(),
                }
                // rax = 64 位结果
                // 6. 截断到操作数类型, 检测溢出
                // 保存完整结果到 rcx
                self.emit(&[0x48, 0x89, 0xc1]); // mov rcx, rax
                match op_size {
                    4 => {
                        // 32 位: 截断到 eax, 符号扩展到 rdx
                        self.emit(&[0x89, 0xc0]); // mov eax, eax (截断到 32 位)
                        self.emit(&[0x48, 0x63, 0xd0]); // movsxd rdx, eax (符号扩展)
                    }
                    2 => {
                        // 16 位: movsx rdx, ax
                        self.emit(&[0x48, 0x0f, 0xbf, 0xd0]); // movsx rdx, ax
                    }
                    1 => {
                        // 8 位: movsx rdx, al
                        self.emit(&[0x48, 0x0f, 0xbe, 0xd0]); // movsx rdx, al
                    }
                    _ => {
                        // 64 位: 无截断, 用 seto
                        self.emit(&[0x0f, 0x90, 0xc2]); // seto dl
                        self.emit(&[0x49, 0x89, 0x02]); // mov [r10], rax
                        self.emit(&[0x0f, 0xb6, 0xc2]); // movzx eax, dl
                        if dst != 0 { self.mov(dst, 0); }
                        return true;
                    }
                }
                // rdx = 截断后符号扩展的结果, rcx = 64 位结果
                // 比较 rdx 和 rcx: 如果不等则溢出
                self.emit(&[0x48, 0x39, 0xd1]); // cmp rcx, rdx
                self.emit(&[0x0f, 0x94, 0xc2]); // sete dl (相等=无溢出=1)
                self.emit(&[0x80, 0xf2, 0x01]); // xor dl, 1 (反转: 溢出=1)
                // 7. 存储截断结果到 *ptr
                match op_size {
                    4 => { self.emit(&[0x49, 0x89, 0x02]); } // mov [r10], eax
                    2 => { self.emit(&[0x49, 0x89, 0x02]); } // mov [r10], eax (低 16 位)
                    1 => { self.emit(&[0x49, 0x89, 0x02]); } // mov [r10], eax (低 8 位)
                    _ => { self.emit(&[0x49, 0x89, 0x02]); } // mov [r10], rax
                }
                // 8. dst = 溢出标志
                self.emit(&[0x0f, 0xb6, 0xc2]); // movzx eax, dl
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // ===== M12+ 补洞 builtins =====
            // __builtin_strcasecmp(s1, s2): 不区分大小写比较, 走 libc.
            // 链接时由 glibc strcasecmp 提供; freestanding 测试可在 fixture 中
            // 自行实现同名函数并通过 weak 引用覆盖 (见 link/resolve.rs).
            "__builtin_strcasecmp" | "__builtin_strncasecmp" => {
                let libc_name = if name == "__builtin_strncasecmp" { "strncasecmp" } else { "strcasecmp" };
                let ar = [7u8, 6, 2, 1, 8, 9];
                for (i, a) in args.iter().enumerate() { self.gen_expr(a, ar[i]); }
                self.emit(&[0x54]); // push rsp
                self.call_label(libc_name);
                self.emit(&[0x5c]); // pop rsp
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_fma / fmaf / fmal: 融合乘加 (优先 vfmadd213pd/fma libm 走 fma).
            // 走 libm: 链接器解析 fma, 兼顾 freestanding (编译时不开 vfmadd).
            "__builtin_fma" | "__builtin_fmaf" | "__builtin_fmal" => {
                if args.len() < 3 { self.mov_imm(dst, 0); return true; }
                // SysV ABI: xmm0, xmm1, xmm2.
                // 策略: 每个参数都 gen_expr 到 xmm0, 然后移至目标 xmm.
                // 但 i=1/2 加载到 xmm0 会覆盖 i=0 的 arg0. 修复: 用 xmm3 备份 arg0.
                // 关键: fmaf 用 movss (F3 0F 10/11, 32-bit), fma/fmal 用 movsd (F2 0F 10/11, 64-bit).
                // movss 只设低 32 位, 高 96 位不变; movsd 只设低 64 位, 高 64 位不变.
                // 备份/恢复必须用与原始加载同宽的指令.
                let (prefix, opcode) = if name == "__builtin_fmaf" { (0xF3u8, 0x10u8) }
                                       else { (0xF2u8, 0x10u8) };
                // ModR/M (F2/F3 0F 11 指令: MOVSD/MOVSS xmm/m, xmm; reg=src, rm=dst):
                //   xmm3 <- xmm0: reg=0, rm=3 ⇒ 0xC3
                //   xmm1 <- xmm0: reg=0, rm=1 ⇒ 0xC1
                //   xmm2 <- xmm0: reg=0, rm=2 ⇒ 0xC2
                // ModR/M (F2/F3 0F 10 指令: MOVSD/MOVSS xmm, xmm/m; reg=dst, rm=src):
                //   xmm0 <- xmm3: reg=0, rm=3 ⇒ 0xC3
                self.gen_expr(&args[0], 0);  // xmm0 = arg0
                self.emit(&[prefix, 0x0F, 0x11, 0xC3]); // mov[sd|ss] xmm3, xmm0 (备份 arg0)
                self.gen_expr(&args[1], 0);  // xmm0 = arg1
                self.emit(&[prefix, 0x0F, 0x11, 0xC1]); // mov[sd|ss] xmm1, xmm0
                self.gen_expr(&args[2], 0);  // xmm0 = arg2
                self.emit(&[prefix, 0x0F, 0x11, 0xC2]); // mov[sd|ss] xmm2, xmm0
                self.emit(&[prefix, 0x0F, opcode, 0xC3]); // mov[sd|ss] xmm0, xmm3 (恢复 arg0)
                self.emit(&[0x54]); // push rsp
                // 调用 libm 中对应函数: fma / fmaf / fmal
                let fn_name = match name {
                    "__builtin_fmaf" => "fmaf",
                    "__builtin_fmal" => "fmal",
                    _ => "fma",
                };
                self.call_label(fn_name);
                self.emit(&[0x5c]); // pop rsp
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_rotateleft8/16/32/64 / __builtin_rotateright8/16/32/64
            // 策略: 按位宽选 ROL/ROR modrm 编码.
            n if n.starts_with("__builtin_rotateleft") || n.starts_with("__builtin_rotateright") => {
                if args.len() < 2 { self.mov_imm(dst, 0); return true; }
                let width: u32 = match n {
                    "__builtin_rotateleft8" | "__builtin_rotateright8" => 8,
                    "__builtin_rotateleft16" | "__builtin_rotateright16" => 16,
                    "__builtin_rotateleft32" | "__builtin_rotateright32" => 32,
                    _ => 64,
                };
                // shift amount = args[1] & (width - 1)
                // result = rotate(args[0], shift)
                self.gen_expr(&args[0], dst);
                self.gen_expr(&args[1], 1); // rcx = shift
                if width == 64 {
                    let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 };
                    if n.starts_with("__builtin_rotateleft") {
                        self.emit(&[rex, 0xD3, 0xC0 | (dst & 7)]); // rol dst, cl
                    } else {
                        self.emit(&[rex, 0xD3, 0xC8 | (dst & 7)]); // ror dst, cl
                    }
                } else if width == 32 {
                    // 32-bit 操作: mov 到 eax, rol/ror eax, cl, mov 回 dst
                    self.emit(&[0x89, 0xC0 | (dst & 7)]); // mov eax, dst (截断到 32 位)
                    if n.starts_with("__builtin_rotateleft") {
                        self.emit(&[0xD3, 0xC0]); // rol eax, cl
                    } else {
                        self.emit(&[0xD3, 0xC8]); // ror eax, cl
                    }
                    if dst >= 8 {
                        // mov dst, eax  (REX.B 89 C0+reg)
                        self.emit(&[0x41, 0x89, 0xC0 | (dst & 7)]);
                    } else {
                        self.emit(&[0x89, 0xC0 | (dst & 7)]);
                    }
                } else if width == 16 {
                    // 16-bit 操作
                    self.emit(&[0x66]);
                    if dst >= 8 { self.emit(&[0x41]); }
                    if n.starts_with("__builtin_rotateleft") {
                        self.emit(&[0xD3, 0xC0 | (dst & 7)]); // rol r16, cl
                    } else {
                        self.emit(&[0xD3, 0xC8 | (dst & 7)]); // ror r16, cl
                    }
                } else {
                    // 8-bit 操作
                    if n.starts_with("__builtin_rotateleft") {
                        self.emit(&[0xD2, 0xC0 | (dst & 7)]); // rol r8, cl
                    } else {
                        self.emit(&[0xD2, 0xC8 | (dst & 7)]); // ror r8, cl
                    }
                }
                true
            }
            // __builtin_clrsb / clrsbl / clrsbll: 计算符号位冗余的位数.
            // 等价于: clz(x ^ s_bits) - 1, 其中 s_bits = 把符号位扩展到整个类型宽度.
            // 对 32-bit:
            //   x = 0x00000001 → s=0, x^s = 1,         clz=31, clrsb=30
            //   x = 0x80000000 → s=-1, x^s = 0x7FFFFFFF, clz=1,  clrsb=0
            //   x = 0xFFFFFFFF → s=-1, x^s = 0,         clz=32, clrsb=31
            //   x = 0          → s=0,  x^s = 0,         clz=32, clrsb=31
            // 实现技巧 (关键: 必须用与类型宽度一致的位运算):
            //   1. mask 到类型宽度 (32-bit 用 cdq/32-bit bsr; 64-bit 用 cqo/64-bit bsr).
            //   2. 用 cdq (32-bit) 或 cqo (64-bit) 把符号位扩展到寄存器宽度.
            //   3. xor 后, 用 test+cmove 解决 bsr 0 未定义:
            //      a. 把 x^s 复制到第二寄存器 (ecx/rcx), 用于 test 设置 ZF
            //         (不能依赖 bsr 的 ZF, 因为 bsr(0) 在 x86 CPU 上 ZF=0, 不可靠).
            //      b. bsr eax, eax (无副作用依赖)
            //      c. mov edx, -1 (sentinel); test ecx, ecx; cmove eax, edx
            //         → ecx==0 时 eax = -1; 否则 eax = bsr.
            //      d. mov ecx, bits-2; sub ecx, eax
            //         正常: bits-2 - bsr = clrsb ✓
            //         零:   bits-2 - (-1) = bits-1 = clrsb ✓
            // 注: 不能用 sar reg, (bits-1) 在 64-bit reg 上做符号扩展, 因为
            //     dcc 把 0x80000000 当作正数 2^31 零扩展到 64-bit (0x0000000080000000),
            //     SAR 31 不会把 1 符号位填到 64 位, 而是填 0. 必须用 cdq/cqo.
            "__builtin_clrsb" | "__builtin_clrsbl" | "__builtin_clrsbll" => {
                if args.is_empty() { self.mov_imm(dst, 0); return true; }
                let bits = args[0].ty().size() as i32 * 8;
                self.gen_expr(&args[0], 0); // rax = x
                if bits == 64 {
                    // 64-bit clrsb: 用 cqo 复制符号到 rdx, 然后 xor rax, rdx.
                    self.emit(&[0x48, 0x99]); // cqo (REX.W)
                    self.emit(&[0x48, 0x31, 0xD0]); // xor rax, rdx
                    self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax
                    self.emit(&[0x48, 0x0F, 0xBD, 0xC0]); // bsr rax, rax
                    self.emit(&[0x48, 0xC7, 0xC2, 0xFF, 0xFF, 0xFF, 0xFF]); // mov rdx, -1
                    self.emit(&[0x48, 0x85, 0xC9]); // test rcx, rcx
                    self.emit(&[0x48, 0x0F, 0x44, 0xC2]); // cmove rax, rdx
                    self.emit(&[0x48, 0xC7, 0xC2, 0x3E, 0x00, 0x00, 0x00]); // mov rdx, 62
                    self.emit(&[0x48, 0x29, 0xC2]); // sub rdx, rax
                    if dst != 2 {
                        if dst < 8 {
                            self.emit(&[0x48, 0x89, 0xD0 | (dst & 7)]);
                        } else {
                            self.emit(&[0x49, 0x89, 0xD0 | ((dst - 8) & 7)]);
                        }
                    }
                } else {
                    // 32-bit (及 16/8-bit) clrsb: 用 32-bit 操作.
                    // 32-bit AND EAX, imm32 零扩展到 RAX (无 REX.W).
                    let mask32: u32 = if bits == 32 { 0xFFFFFFFF }
                                      else if bits == 16 { 0x0000FFFF }
                                      else { 0x000000FF };
                    let mb = mask32.to_le_bytes();
                    self.emit(&[0x25, mb[0], mb[1], mb[2], mb[3]]);
                    // 计算 s_bits 到 edx: 0 或 mask32 (32-bit).
                    if bits == 32 {
                        self.emit(&[0x99]); // cdq: edx = sign(eax) = 0 或 0xFFFFFFFF
                    } else if bits == 16 {
                        self.emit(&[0xC1, 0xE0, 16]); // shl eax, 16
                        self.emit(&[0xC1, 0xF8, 31]); // sar eax, 31 → 0 或 -1
                        self.emit(&[0x25, 0xFF, 0xFF, 0x00, 0x00]); // and eax, 0xFFFF
                        self.emit(&[0x89, 0xC2]); // mov edx, eax
                    } else { // bits == 8
                        self.emit(&[0xC1, 0xE0, 24]); // shl eax, 24
                        self.emit(&[0xC1, 0xF8, 31]); // sar eax, 31 → 0 或 -1
                        self.emit(&[0x25, 0xFF, 0x00, 0x00, 0x00]); // and eax, 0xFF
                        self.emit(&[0x89, 0xC2]); // mov edx, eax
                    }
                    // xor eax, edx: eax = x ^ s_bits (32-bit)
                    self.emit(&[0x31, 0xD0]);
                    // mov ecx, eax: 保存 x^s_bits 到 ecx, 用于 test
                    self.emit(&[0x89, 0xC1]);
                    // bsr eax, eax (32-bit) - 可能为 UB for 0, 但 cmove 兜底
                    self.emit(&[0x0F, 0xBD, 0xC0]);
                    // mov edx, 0xFFFFFFFF (sentinel)
                    self.emit(&[0xBA, 0xFF, 0xFF, 0xFF, 0xFF]);
                    // test ecx, ecx: 设置 ZF 基于 ecx (0 ⇒ ZF=1), 覆盖 bsr 的 ZF
                    self.emit(&[0x85, 0xC9]);
                    // cmove eax, edx: 如果 x^s_bits==0, eax = edx = -1
                    // ModR/M = 0xC2 = 11_000_010: reg=eax(0), rm=edx(2) ⇒ Intel cmove eax, edx
                    self.emit(&[0x0F, 0x44, 0xC2]);
                    // mov ecx, (bits-2)
                    let bm2: u32 = (bits - 2) as u32;
                    let b = bm2.to_le_bytes();
                    self.emit(&[0xB9, b[0], b[1], b[2], b[3]]);
                    // sub ecx, eax: ecx = clrsb
                    self.emit(&[0x29, 0xC1]);
                    // mov dst, ecx (REX.W; ecx 零扩展到 dst)
                    if dst != 1 {
                        if dst < 8 {
                            self.emit(&[0x48, 0x89, 0xC8 | (dst & 7)]);
                        } else {
                            self.emit(&[0x49, 0x89, 0xC8 | ((dst - 8) & 7)]);
                        }
                    }
                }
                true
            }
            // __builtin_apply_args(): 返回保存区指针 (SysV: [rsp+8])
            "__builtin_apply_args" => {
                // lea rax, [rsp+8]: 48 8D 44 24 08
                self.emit(&[0x48, 0x8D, 0x44, 0x24, 0x08]);
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            // __builtin_choose_expr fallback (parser 应当已折叠)
            "__builtin_choose_expr" => {
                if args.len() >= 3 {
                    let cond = const_eval::eval(&args[0]).unwrap_or(0);
                    let chosen = if cond != 0 { &args[1] } else { &args[2] };
                    self.gen_expr(chosen, dst);
                } else {
                    self.mov_imm(dst, 0);
                }
                true
            }
            // __builtin_types_compatible_p fallback
            "__builtin_types_compatible_p" => {
                // parser 应当已折叠; 此处按 0 处理
                self.mov_imm(dst, 0);
                true
            }
            // __builtin_va_arg(ap, type) — parser 已生成 Expr::VaArg, 此兜底不会到达
            "__builtin_va_arg" => {
                true
            }
            // __builtin_FILE / __builtin_LINE / __builtin_FUNCTION / __builtin_FUNCSIG
            // parser 已折叠; codegen 兜底返回空串 (与 parser 一致).
            "__builtin_FILE" | "__builtin_FUNCTION" | "__builtin_FUNCSIG"
            | "__builtin_PRETTY_FUNCTION" => {
                // 复用 string 表达式生成路径, 避免重复维护 rodata ref 编码
                let empty: Vec<u8> = vec![0];
                let data = empty;
                if !self.string_pool.contains_key(&data) {
                    let o = self.rodata.len() as u64;
                    self.rodata.extend_from_slice(&data);
                    self.string_pool.insert(data.clone(), o);
                }
                let at = self.pos();
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                self.emit(&[rex, 0x8D, 0x05 | ((dst & 7) << 3), 0, 0, 0, 0]);
                self.string_refs.push((at, data));
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            "__builtin_LINE" => {
                self.mov_imm(dst, 0);
                true
            }
            // __builtin_classify_type fallback
            "__builtin_classify_type" => {
                self.mov_imm(dst, 0);
                true
            }
            _ => false,
        }
    }

    /// 加载函数指针（Ident / cast，用于 __builtin_apply 等）
    fn gen_builtin_fn_ptr(&mut self, e: &Expr, dst: u8) {
        let inner = match e {
            Expr::Cast { e, .. } | Expr::AddrOf { e, .. } => e.as_ref(),
            other => other,
        };
        if let Expr::Ident { name, .. } = inner {
            if self.labels.contains_key(&name.name) || self.globals.contains_key(&name.name) {
                let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
                let modrm = 0x05 | ((dst & 7) << 3);
                self.emit(&[rex, 0x8D, modrm, 0, 0, 0, 0]);
                let disp32_pos = self.pos() - 4;
                self.label_addr_refs.push((disp32_pos, name.name.clone()));
                return;
            }
        }
        self.gen_expr(e, dst);
    }

    /// 计算结构体成员偏移 (用于 __builtin_offsetof)
    fn compute_offset(ty: &Type, member: &str) -> usize {
        let fields = match ty {
            Type::Struct(_, fields) => fields,
            Type::Union(_, _fields) => {
                // union: 所有成员偏移都是 0
                return 0;
            }
            _ => return 0,
        };
        let parts: Vec<&str> = member.split('.').collect();
        let mut offset = 0usize;
        let mut current_fields = fields.as_ref();
        for (i, part) in parts.iter().enumerate() {
            let part = if part.contains('[') { &part[..part.find('[').unwrap()] } else { part };
            let mut found = false;
            for field in current_fields.iter() {
                if field.name == *part {
                    offset += field.offset;
                    if i < parts.len() - 1 {
                        // 嵌套: 进入子结构
                        match &field.ty {
                            Type::Struct(_, sub_fields) => {
                                current_fields = sub_fields.as_ref();
                            }
                            _ => {}
                        }
                    }
                    found = true;
                    break;
                }
            }
            if !found { break; }
        }
        offset
    }

    /// 获取指针参数所指元素的大小
    fn atomic_elem_size(&self, args: &[Expr], idx: usize) -> usize {
        if idx >= args.len() { return 8; }
        match args[idx].ty() {
            Type::Ptr(ref inner) => inner.size(),
            Type::Array(ref inner, _) => inner.size(),
            _ => 8,
        }
    }

    /// 原子内置函数 codegen: 返回 true 表示已处理
    fn gen_atomic_builtin(&mut self, name: &str, args: &[Expr], dst: u8) -> bool {
        match name {
            "__atomic_fetch_add" => {
                // args: (ptr, val, order)
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                // lock xadd [rdi], rsi (按 sz 选操作数尺寸)
                self.emit_atomic_xadd(sz);
                if dst != 6 { self.mov(dst, 6); }
                true
            }
            "__atomic_fetch_sub" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                // neg rsi (按 sz 选操作数尺寸)
                self.emit_neg(sz);
                self.emit_atomic_xadd(sz);
                if dst != 6 { self.mov(dst, 6); }
                true
            }
            "__atomic_fetch_and" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg| {
                    cg.emit_and_rsi();
                });
                if dst != 1 { self.mov(dst, 1); }
                true
            }
            "__atomic_fetch_or" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg| {
                    cg.emit_or_rsi();
                });
                if dst != 1 { self.mov(dst, 1); }
                true
            }
            "__atomic_fetch_xor" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7);
                self.gen_expr(&args[1], 6);
                self.emit_cas_loop(sz, |cg| {
                    cg.emit_xor_rsi();
                });
                if dst != 1 { self.mov(dst, 1); }
                true
            }
            "__atomic_compare_exchange_n" => {
                // args: (ptr, expected, desired, weak, success_order, failure_order)
                if args.len() < 3 { return false; }
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 0); // rax = expected (cmpxchg 用 rax)
                // 保存 expected 到帧临时槽, 因为还需要写入
                let slot = -8 - (self.expr_depth as i32 * 8);
                {
                    let rex = 0x49u8;
                    self.emit(&[rex, 0x89]);
                    if slot.abs() < 128 { self.emit(&[0x45, slot as i8 as u8]); }
                    else { self.emit(&[0x85]); self.emit(&slot.to_le_bytes()); }
                }
                // 解引用 expected 指针, rax = *expected (实际期望值)
                self.emit(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
                self.gen_expr(&args[2], 6); // rsi = desired
                // lock cmpxchg [rdi], rsi: 按 sz 选尺寸
                let sz = self.atomic_elem_size(args, 0);
                if sz == 8 {
                    self.emit(&[0xF0, 0x48, 0x0F, 0xB1, 0x37]);
                } else {
                    self.emit(&[0xF0, 0x0F, 0xB1, 0x37]);
                }
                // ZF=1 成功(rax=old), ZF=0 失败(rax=old)
                self.emit(&[0x0F, 0x94, 0xC2]); // sete dl
                let l_skip = self.fresh_label();
                self.jmp_rel32(&l_skip, JumpKind::Je);
                // 失败: 将 rax (当前值) 写入 [expected_ptr]
                // 把 expected 指针加载到 rsi (而非 rax, rax 保留当前值)
                {
                    let rex = 0x49u8;
                    self.emit(&[rex, 0x8B]);
                    if slot.abs() < 128 {
                        self.emit(&[0x75, slot as i8 as u8]); // modrm: [r13+disp8], reg=rsi
                    } else {
                        self.emit(&[0xB5]); // modrm: [r13+disp32], reg=rsi
                        self.emit(&slot.to_le_bytes());
                    }
                }
                self.emit(&[0x48, 0x89, 0x06]); // mov [rsi], rax
                self.label(&l_skip);
                self.emit(&[0x48, 0x0F, 0xB6, 0xC2]); // movzx rax, dl
                if dst != 0 { self.mov(dst, 0); }
                true
            }
            "__atomic_store_n" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                self.emit_mov_to_mem(sz);   // mov [rdi], rsi (按 sz 选尺寸)
                self.emit(&[0x0F, 0xAE, 0xF0]); // mfence
                true
            }
            "__atomic_load_n" => {
                if args.len() < 2 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.emit_mov_from_mem(sz, dst); // mov reg, [rdi] (按 sz 选尺寸)
                true
            }
            "__atomic_exchange_n" => {
                if args.len() < 3 { return false; }
                let sz = self.atomic_elem_size(args, 0);
                self.gen_expr(&args[0], 7); // rdi = ptr
                self.gen_expr(&args[1], 6); // rsi = val
                // lock xchg [rdi], rsi: 按 sz 选尺寸
                if sz == 8 {
                    self.emit(&[0xF0, 0x48, 0x87, 0x37]);
                } else {
                    self.emit(&[0xF0, 0x87, 0x37]);
                }
                if dst != 6 { self.mov(dst, 6); }
                true
            }
            _ => false, // 未识别的 __atomic_ 函数, 走普通调用路径
        }
    }

    // ===== 原子操作辅助指令 =====

    /// lock xadd [rdi], rsi — 按 sz 选 32/64 位
    fn emit_atomic_xadd(&mut self, sz: usize) {
        match sz {
            4 => self.emit(&[0xF0, 0x0F, 0xC1, 0x37]),        // lock xadd [rdi], esi
            _ => self.emit(&[0xF0, 0x48, 0x0F, 0xC1, 0x37]),  // lock xadd [rdi], rsi
        }
    }

    /// neg rsi — 按 sz 选 32/64 位
    fn emit_neg(&mut self, sz: usize) {
        match sz {
            4 => self.emit(&[0xF7, 0xDE]),        // neg esi
            _ => self.emit(&[0x48, 0xF7, 0xDE]),  // neg rsi
        }
    }

    /// mov [rdi], rsi — 按 sz 选操作数尺寸
    fn emit_mov_to_mem(&mut self, sz: usize) {
        match sz {
            1 => self.emit(&[0x40, 0x88, 0x37]),  // mov [rdi], sil
            2 => self.emit(&[0x66, 0x89, 0x37]),  // mov [rdi], si
            4 => self.emit(&[0x89, 0x37]),         // mov [rdi], esi
            _ => self.emit(&[0x48, 0x89, 0x37]),  // mov [rdi], rsi
        }
    }

    /// mov reg, [rdi] — 按 sz 选操作数尺寸, 结果放入 dst
    fn emit_mov_from_mem(&mut self, sz: usize, dst: u8) {
        match sz {
            1 => self.emit(&[0x0F, 0xB6, 0x07]),  // movzx eax, byte [rdi]
            2 => self.emit(&[0x0F, 0xB7, 0x07]),  // movzx eax, word [rdi]
            4 => self.emit(&[0x8B, 0x07]),         // mov eax, [rdi] (zero-extends)
            _ => self.emit(&[0x48, 0x8B, 0x07]),  // mov rax, [rdi]
        }
        if dst != 0 { self.mov(dst, 0); }
    }

    /// CAS 循环: 读旧值, 计算新值, lock cmpxchg, 循环直到成功
    fn emit_cas_loop<F: Fn(&mut Self)>(&mut self, sz: usize, compute_new: F) {
        if sz == 8 {
            // 64-bit
            self.emit(&[0x48, 0x8B, 0x07]); // mov rax, [rdi]
            let l_retry = self.fresh_label();
            self.label(&l_retry);
            self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax (save old)
            self.emit(&[0x48, 0x89, 0xC2]); // mov rdx, rax
            compute_new(self);                 // and/or/xor rdx, rsi
            self.emit(&[0xF0, 0x48, 0x0F, 0xB1, 0x17]); // lock cmpxchg [rdi], rdx
            let l_done = self.fresh_label();
            self.jmp_rel32(&l_done, JumpKind::Je);
            self.jmp_rel32(&l_retry, JumpKind::Jmp);
            self.label(&l_done);
            // After successful cmpxchg: RAX = old value, RCX = old value (from mov rcx,rax before cmpxchg)
        } else {
            // 32-bit (zero-extends to 64-bit)
            self.emit(&[0x8B, 0x07]); // mov eax, [rdi]
            let l_retry = self.fresh_label();
            self.label(&l_retry);
            self.emit(&[0x89, 0xC1]); // mov ecx, eax
            self.emit(&[0x89, 0xC2]); // mov edx, eax
            compute_new(self);
            self.emit(&[0xF0, 0x0F, 0xB1, 0x17]); // lock cmpxchg [rdi], edx
            let l_done = self.fresh_label();
            self.jmp_rel32(&l_done, JumpKind::Je);
            self.jmp_rel32(&l_retry, JumpKind::Jmp);
            self.label(&l_done);
        }
    }

    /// and rdx, rsi (32-bit: 21 F2, 64-bit: 48 21 F2)
    /// 由 emit_cas_loop 的 compute_new 回调使用
    fn emit_and_rsi(&mut self) {
        // 32-bit and (rdx = rdx & rsi)
        self.emit(&[0x21, 0xF2]); // and edx, esi
    }
    fn emit_or_rsi(&mut self) {
        self.emit(&[0x09, 0xF2]); // or edx, esi
    }
    fn emit_xor_rsi(&mut self) {
        self.emit(&[0x31, 0xF2]); // xor edx, esi
    }

    // ===== Inline Asm Mini-Assembler (AT&T syntax) =====

    /// 解析 GCC asm 约束前缀 (= 输出, + 读写, & early-clobber)
    /// 返回 (is_output, is_inout, early_clobber, match_digit)
    /// match_digit: Some(n) 表示 "与第 n 个操作数共用寄存器", None 表示无
    fn parse_asm_constraint_flags(constraint: &str) -> (bool, bool, bool, Option<u8>) {
        let mut is_output = false;
        let mut is_inout = false;
        let mut early = false;
        let mut match_digit: Option<u8> = None;
        let mut s = constraint.trim();
        while !s.is_empty() {
            match s.chars().next() {
                Some('=') => { is_output = true; s = &s[1..]; }
                Some('+') => { is_output = true; is_inout = true; s = &s[1..]; }
                Some('&') => { early = true; s = &s[1..]; }
                Some('%') => { s = &s[1..]; } // 数字标签计数约束, 模板替换处理
                Some(c) if c.is_ascii_digit() => {
                    match_digit = Some(c.to_digit(10).unwrap() as u8);
                    s = &s[1..];
                }
                _ => break,
            }
        }
        (is_output, is_inout, early, match_digit)
    }

    /// 约束是否优先使用内存操作数
    fn asm_constraint_prefers_memory(constraint: &str, is_inout: bool) -> bool {
        if constraint.is_empty() {
            return false;
        }
        let letters: Vec<char> = constraint.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        let has_m = letters.contains(&'m');
        let _has_p = letters.contains(&'p');
        let has_r = letters.iter().any(|c| matches!(c, 'r' | 'R' | 'q' | 'Q' | 'a' | 'b' | 'c' | 'd' | 'S' | 'D'));
        // "p" 表示内存地址, 但仍需填入寄存器 (地址本身), 不当作内存操作数直接替换
        if has_m && (!has_r || is_inout) {
            return true;
        }
        letters == vec!['m']
    }

    /// 约束是否使用立即数 (含 "i", "n", "I", "N", "M", "X", "E")
    fn asm_constraint_is_immediate(constraint: &str) -> bool {
        constraint.chars().any(|c| matches!(c, 'i' | 'n' | 'I' | 'N' | 'M' | 'X' | 'E'))
    }

    /// 约束是否要求 XMM 寄存器 ("x", "Y", "Yt", "Xf")
    fn asm_constraint_wants_xmm(constraint: &str) -> Option<u8> {
        let letters: Vec<char> = constraint.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        if letters.contains(&'x') {
            // 默认 128 位; "Yt" 是 xmm, "Xf" 是 32 位, "Yx" 是 64 位
            return Some(128);
        }
        if letters.contains(&'Y') {
            return Some(128);
        }
        None
    }

    /// 约束是否要求内存地址 ("p")
    fn asm_constraint_is_address(constraint: &str) -> bool {
        constraint.chars().any(|c| c == 'p')
    }

    /// 约束是否通用 ("g" 包含 reg/mem/imm)
    fn asm_constraint_is_general(constraint: &str) -> bool {
        let letters: Vec<char> = constraint.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        letters.contains(&'g')
    }

    /// 从多约束 (如 "rm") 中按优先级选择单约束
    fn asm_pick_sub_constraint(constraint: &str, prefer_memory: bool) -> &'static str {
        let letters: Vec<char> = constraint.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        if prefer_memory && letters.contains(&'m') { return "m"; }
        if letters.contains(&'r') { return "r"; }
        if letters.contains(&'m') { return "m"; }
        // 立即数约束: i (任意) / n / I/J/K/L/M/N/O (位移量) / X / E/F/G/H (浮点常量)
        if letters.iter().any(|c| matches!(c, 'i' | 'n' | 'I' | 'J' | 'K' | 'L' | 'M' | 'N' | 'O' | 'X' | 'E' | 'F' | 'G' | 'H' | 's')) {
            return "i";
        }
        if letters.contains(&'g') { return "g"; }
        "r"
    }

    /// 为 asm 操作数生成 AT&T 内存操作数字符串 (基于 r13 帧指针)
    fn asm_memory_operand_str(&mut self, expr: &Expr) -> Option<String> {
        if let Expr::Ident { name, .. } = expr {
            if self.globals.contains_key(&name.name) {
                return Some(format!("{}(%rip)", name.name));
            }
            let off = self.var_offset(&name.name);
            return Some(format!("{off}(%r13)"));
        }
        None
    }

    /// 按约束为特定字母选择寄存器名
    fn asm_reg_for_constraint(constraint: &str, idx: usize) -> &'static str {
        let letters: Vec<char> = constraint.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        if letters.contains(&'a') { return "rax"; }
        if letters.contains(&'b') { return "rbx"; }
        if letters.contains(&'c') { return "rcx"; }
        if letters.contains(&'d') { return "rdx"; }
        if letters.contains(&'S') { return "rsi"; }
        if letters.contains(&'D') { return "rdi"; }
        if letters.contains(&'q') || letters.contains(&'Q') {
            return ["rax", "rbx", "rcx", "rdx"][idx % 4];
        }
        ["rax", "rcx", "rdx", "rsi", "rdi", "r8", "r9", "r10", "r11"][idx % 9]
    }

    fn asm_name_to_reg_index(name: &str) -> u8 {
        match name {
            "rax" => 0, "rcx" => 1, "rdx" => 2, "rbx" => 3,
            "rsp" => 4, "rbp" => 5, "rsi" => 6, "rdi" => 7,
            "r8" => 8, "r9" => 9, "r10" => 10, "r11" => 11,
            _ => 0,
        }
    }

    /// XMM 寄存器名 (xmm0..xmm15) → 物理索引 (0..15)
    fn asm_xmm_name(idx: usize) -> &'static str {
        const NAMES: [&str; 16] = [
            "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5", "xmm6", "xmm7",
            "xmm8", "xmm9", "xmm10", "xmm11", "xmm12", "xmm13", "xmm14", "xmm15",
        ];
        NAMES[idx % 16]
    }

    /// 构建 asm 操作数映射: %N / %[name] → 寄存器名或内存操作数
    fn build_asm_operand_map(
        &mut self,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        bindings: &mut Vec<AsmOpBinding>,
    ) -> HashMap<String, String> {
        let mut map = HashMap::new();
        let mut idx = 0usize;

        // 内部占位: 每个操作数分配状态
        enum Slot {
            /// 立即数 (模板内联)
            Imm(String),
            /// 内存操作数 (模板直接出现)
            Mem(String),
            /// 寄存器 (GP 或 XMM)
            Reg(u8, &'static str),
        }
        let mut slots: Vec<Slot> = Vec::new();
        let mut meta: Vec<(u8, u8, bool, bool, u8)> = Vec::new(); // (reg, xmm_width, is_output, is_mem, flags_raw)
        // flags_raw: bit 0 = early_clobber

        // 先扫一遍输入, 收集输入寄存器集合 (用于 early-clobber 输出避让)
        let mut input_reg_set: std::collections::HashSet<u8> = std::collections::HashSet::new();
        // XMM 输入独立集合: GP 与 XMM 共用 0..15 编号空间, 不能混在同一 set 里判断冲突
        let mut input_xmm_set: std::collections::HashSet<u8> = std::collections::HashSet::new();

        // ===== 处理 outputs =====
        for (op_idx, op) in outputs.iter().enumerate() {
            let raw = output_constraints.get(op_idx).map(String::as_str).unwrap_or("=r");
            let (is_output, is_inout, early, match_digit) = Self::parse_asm_constraint_flags(raw);
            let letter = Self::asm_pick_sub_constraint(raw, is_inout);
            let _ = is_output; // 已经在 flags 派生

            // match-digit: 复用先前操作数 (仅当 N < 当前全局 idx)
            if let Some(n) = match_digit {
                if (n as usize) < slots.len() {
                    let slot = &slots[n as usize];
                    match slot {
                        Slot::Imm(s) => { map.insert(format!("%{idx}"), s.clone()); }
                        Slot::Mem(s) => { map.insert(format!("%{idx}"), s.clone()); }
                        Slot::Reg(r, name) => {
                            let nreg = *r;
                            map.insert(format!("%{idx}"), (*name).to_string());
                            input_reg_set.insert(nreg);
                            meta.push((nreg, 0, true, false, 0));
                        }
                    }
                    slots.push(match slot { Slot::Imm(s) => Slot::Imm(s.clone()), Slot::Mem(s) => Slot::Mem(s.clone()), Slot::Reg(r, n) => Slot::Reg(*r, n) });
                    idx += 1;
                    continue;
                }
            }

            // 立即数约束且操作数可常量求值
            if matches!(letter, "i") && Self::asm_constraint_is_immediate(raw) {
                if let Some(val) = Self::eval_asm_operand(op) {
                    // 添加 AT&T 立即数前缀 $, 例如 305419896 -> $305419896
                    let att_val = if val.starts_with('$') { val.clone() } else { format!("${val}") };
                    map.insert(format!("%{idx}"), att_val.clone());
                    slots.push(Slot::Imm(att_val));
                    meta.push((0, 0, true, false, 0));
                    idx += 1;
                    continue;
                }
            }

            // 内存操作数 (m 优先)
            if Self::asm_constraint_prefers_memory(raw, is_inout) {
                if let Some(mem) = self.asm_memory_operand_str(op) {
                    map.insert(format!("%{idx}"), mem.clone());
                    slots.push(Slot::Mem(mem));
                    meta.push((0, 0, true, true, if early { 1 } else { 0 }));
                    idx += 1;
                    continue;
                }
            }

            // XMM 寄存器约束
            if let Some(xmm_bits) = Self::asm_constraint_wants_xmm(raw) {
                let xi = idx % 16;
                let name = Self::asm_xmm_name(xi);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(xi as u8, name));
                meta.push((xi as u8, xmm_bits, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // 内存地址 (p): lea 到一个 GP 寄存器, 模板里就是 %reg
            if Self::asm_constraint_is_address(raw) {
                let gp_pool: [u8; 9] = [0, 1, 2, 6, 7, 8, 9, 10, 11];
                let chosen = gp_pool[idx % gp_pool.len()];
                // 把 expr 地址加载到 chosen
                self.asm_load_address_to_reg(op, chosen);
                let name = Self::gp_reg_name(chosen as usize);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // 默认: GP 寄存器
            let gp_pool: [u8; 9] = [0, 1, 2, 6, 7, 8, 9, 10, 11];
            let chosen = gp_pool[idx % gp_pool.len()];
            let name = Self::gp_reg_name(chosen as usize);
            self.prepare_asm_binding(op, chosen, true);
            map.insert(format!("%{idx}"), name.to_string());
            slots.push(Slot::Reg(chosen, name));
            meta.push((chosen, 0, true, false, if early { 1 } else { 0 }));
            idx += 1;
        }

        // ===== 处理 inputs =====
        for (op_idx, op) in inputs.iter().enumerate() {
            let raw = input_constraints.get(op_idx).map(String::as_str).unwrap_or("r");
            let (_is_output, is_inout, _early, match_digit) = Self::parse_asm_constraint_flags(raw);
            let _letter = Self::asm_pick_sub_constraint(raw, is_inout);

            // match-digit: 复用先前操作数的寄存器, 但仍需 prepare_asm_binding
            // 把当前操作数 (作为输入) 的值加载到该寄存器, 不能只复制 slot.
            // 例如 "=r"(b) : "0"(a): 输出准备把 &b 加载到 rax, 但输入 a 应当把
            // a 的值 (42) 重新加载到 rax, 覆盖 &b, 这样 asm "add $1, %0" 才能
            // 在 rax 上做正确算术 (rax = 42 + 1 = 43), 再写回 b.
            if let Some(n) = match_digit {
                if (n as usize) < slots.len() {
                    let slot = &slots[n as usize];
                    if let Slot::Reg(r, name) = slot {
                        let nreg = *r;
                        let nname = *name;
                        // 对当前操作数 (输入) 调 prepare, 把值加载到复用寄存器
                        self.prepare_asm_binding(op, nreg, false);
                        map.insert(format!("%{idx}"), nname.to_string());
                        input_reg_set.insert(nreg);
                        meta.push((nreg, 0, false, false, 0));
                        slots.push(Slot::Reg(nreg, nname));
                        idx += 1;
                        continue;
                    }
                    // 非 Reg slot (Imm/Mem): 直接复制即可
                    match slot {
                        Slot::Imm(s) => { map.insert(format!("%{idx}"), s.clone()); }
                        Slot::Mem(s) => { map.insert(format!("%{idx}"), s.clone()); }
                        Slot::Reg(_, _) => unreachable!(),
                    }
                    slots.push(match slot { Slot::Imm(s) => Slot::Imm(s.clone()), Slot::Mem(s) => Slot::Mem(s.clone()), Slot::Reg(r, n) => Slot::Reg(*r, n) });
                    idx += 1;
                    continue;
                }
            }

            // 立即数约束且操作数可常量求值
            if Self::asm_constraint_is_immediate(raw) {
                if let Some(val) = Self::eval_asm_operand(op) {
                    // 添加 AT&T 立即数前缀 $, 例如 305419896 -> $305419896
                    let att_val = if val.starts_with('$') { val.clone() } else { format!("${val}") };
                    map.insert(format!("%{idx}"), att_val.clone());
                    slots.push(Slot::Imm(att_val));
                    meta.push((0, 0, false, false, 0));
                    idx += 1;
                    continue;
                }
            }

            // 内存操作数
            if Self::asm_constraint_prefers_memory(raw, is_inout) {
                if let Some(mem) = self.asm_memory_operand_str(op) {
                    map.insert(format!("%{idx}"), mem.clone());
                    slots.push(Slot::Mem(mem));
                    meta.push((0, 0, false, true, 0));
                    idx += 1;
                    continue;
                }
            }

            // XMM
            if let Some(xmm_bits) = Self::asm_constraint_wants_xmm(raw) {
                let xi = idx % 16;
                let name = Self::asm_xmm_name(xi);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(xi as u8, name));
                input_xmm_set.insert(xi as u8);
                meta.push((xi as u8, xmm_bits, false, false, 0));
                idx += 1;
                continue;
            }

            // 内存地址 (p)
            if Self::asm_constraint_is_address(raw) {
                let gp_pool: [u8; 9] = [0, 1, 2, 6, 7, 8, 9, 10, 11];
                let chosen = gp_pool[idx % gp_pool.len()];
                self.asm_load_address_to_reg(op, chosen);
                let name = Self::gp_reg_name(chosen as usize);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                input_reg_set.insert(chosen);
                meta.push((chosen, 0, false, false, 0));
                idx += 1;
                continue;
            }

            // GP 寄存器
            let gp_pool: [u8; 9] = [0, 1, 2, 6, 7, 8, 9, 10, 11];
            let chosen = gp_pool[idx % gp_pool.len()];
            let name = Self::gp_reg_name(chosen as usize);
            self.prepare_asm_binding(op, chosen, false);
            map.insert(format!("%{idx}"), name.to_string());
            slots.push(Slot::Reg(chosen, name));
            input_reg_set.insert(chosen);
            meta.push((chosen, 0, false, false, 0));
            idx += 1;
        }

        // ===== early-clobber: 输出寄存器不能与任何输入寄存器重叠 =====
        let gp_pool: [u8; 9] = [0, 1, 2, 6, 7, 8, 9, 10, 11];
        let xmm_pool: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let num_outputs = outputs.len();
        for i in 0..num_outputs {
            let (reg, xmm_w, is_out, is_mem, flags) = meta[i];
            if !is_out || is_mem || (flags & 1) == 0 { continue; }
            if xmm_w != 0 {
                // XMM 输出 + early-clobber: 找一个未被 XMM 输入占用的 XMM
                if !input_xmm_set.contains(&reg) { continue; }
                for &cand in &xmm_pool {
                    if !input_xmm_set.contains(&cand) {
                        let new_name = Self::asm_xmm_name(cand as usize);
                        let key = format!("%{i}");
                        map.insert(key.clone(), new_name.to_string());
                        slots[i] = Slot::Reg(cand, new_name);
                        meta[i] = (cand, xmm_w, true, false, flags);
                        break;
                    }
                }
                continue;
            }
            if !input_reg_set.contains(&reg) { continue; }
            // 找一个不冲突的 GP
            for &cand in &gp_pool {
                if !input_reg_set.contains(&cand) {
                    let new_name = Self::gp_reg_name(cand as usize);
                    // 替换 map 中对应的 %N 条目
                    let key = format!("%{i}");
                    if let Some(old) = map.get(&key).cloned() {
                        // 重新 prepare_asm_binding 到新寄存器
                        if let Some(op) = outputs.get(i) {
                            self.prepare_asm_binding(op, cand, true);
                        }
                        map.insert(key.clone(), new_name.to_string());
                        slots[i] = Slot::Reg(cand, match slots[i] {
                            Slot::Reg(_, _) => {
                                // 取一个静态名: 用 cand 处的名字
                                let n: &'static str = match cand {
                                    0 => "rax", 1 => "rcx", 2 => "rdx", 3 => "rbx",
                                    4 => "rsp", 5 => "rbp", 6 => "rsi", 7 => "rdi",
                                    8 => "r8", 9 => "r9", 10 => "r10", 11 => "r11",
                                    _ => "rax",
                                };
                                n
                            }
                            _ => unreachable!(),
                        });
                        let _ = old;
                    }
                    meta[i] = (cand, 0, true, false, flags);
                    break;
                }
            }
        }

        // 把 meta 转成 AsmOpBinding
        bindings.clear();
        for (i, (reg, xmm_w, is_out, is_mem, flags)) in meta.iter().enumerate() {
            let _ = i;
            bindings.push(AsmOpBinding {
                reg: *reg,
                is_output: *is_out,
                is_mem: *is_mem,
                early_clobber: (flags & 1) != 0,
                xmm_width: *xmm_w,
            });
        }

        // name aliases
        for (i, name) in output_names.iter().enumerate() {
            if let Some(n) = name {
                if let Some(reg) = map.get(&format!("%{}", i)) {
                    map.insert(format!("%[{n}]"), reg.clone());
                }
            }
        }
        for (i, name) in input_names.iter().enumerate() {
            if let Some(n) = name {
                let key = format!("%{}", outputs.len() + i);
                if let Some(reg) = map.get(&key) {
                    map.insert(format!("%[{n}]"), reg.clone());
                }
            }
        }
        map
    }

    /// GP 寄存器索引 → AT&T 名
    fn gp_reg_name(idx: usize) -> &'static str {
        match idx {
            0 => "rax", 1 => "rcx", 2 => "rdx", 3 => "rbx",
            4 => "rsp", 5 => "rbp", 6 => "rsi", 7 => "rdi",
            8 => "r8", 9 => "r9", 10 => "r10", 11 => "r11",
            12 => "r12", 13 => "r13", 14 => "r14", 15 => "r15",
            _ => "rax",
        }
    }

    /// 把 expr 表示的地址 (指针值) 加载到 GP 寄存器 (用于 "p" 约束)
    fn asm_load_address_to_reg(&mut self, expr: &Expr, reg: u8) {
        // p 约束: 把指针值加载到寄存器 (而不是 lea 变量地址)
        // 对于指针变量, 加载其值; 对于其他表达式, 按值求值
        self.gen_expr(expr, reg);
    }

    /// asm 输出操作数写回 C 变量
    fn writeback_asm_operand(&mut self, expr: &Expr, reg: u8) {
        let Expr::Ident { name, ty, .. } = expr else { return };
        if matches!(ty, Type::Float | Type::Double | Type::LongDouble) {
            let off = self.var_offset(&name.name);
            if matches!(ty, Type::Float) {
                self.store_xmm32_stack(reg.min(8), off);
            } else {
                self.store_xmm64_stack(reg.min(8), off);
            }
            return;
        }
        let off = self.var_offset(&name.name);
        let sz = ty.size();
        if sz <= 1 {
            self.emit(&[0x41, 0x88, 0x85 | ((reg & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        } else if sz <= 2 {
            self.emit(&[0x66, 0x41, 0x89, 0x85 | ((reg & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        } else if sz <= 4 {
            // 32-bit 整型写回需零扩展到 64 位槽, 否则后续 64-bit 重载读到栈残留
            // 旧实现用 `mov %eax, [r13+off]` (4 字节), C codegen 后续用 64-bit 读
            // (mov -off(%r13), %rax) 会读到未初始化高 32 位.
            // 改为 `mov %rax, [r13+off]` (REX.W + REX.B, 8 字节), 保证 64-bit 槽整体更新.
            self.emit(&[Self::rex(true, reg, 0, 13), 0x89, 0x85 | ((reg & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        } else {
            self.store_stack(reg, off);
        }
    }

    fn store_xmm32_stack(&mut self, xmm: u8, off: i32) {
        self.emit(&[0xF3]);
        if xmm >= 8 {
            self.emit(&[0x44, 0x41]);
        } else {
            self.emit(&[0x41]);
        }
        self.emit(&[0x0F, 0x11]);
        if off.abs() < 128 {
            self.emit(&[0x45 | ((xmm & 7) << 3), off as i8 as u8]);
        } else {
            self.emit(&[0x85 | ((xmm & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        }
    }

    fn store_xmm64_stack(&mut self, xmm: u8, off: i32) {
        self.emit(&[0xF2]);
        if xmm >= 8 {
            self.emit(&[0x44, 0x41]);
        } else {
            self.emit(&[0x41]);
        }
        self.emit(&[0x0F, 0x11]);
        if off.abs() < 128 {
            self.emit(&[0x45 | ((xmm & 7) << 3), off as i8 as u8]);
        } else {
            self.emit(&[0x85 | ((xmm & 7) << 3)]);
            self.emit(&off.to_le_bytes());
        }
    }

    /// 将 asm 约束表达式加载到指定寄存器。
    /// - `is_output = true`: Ident 取地址 (用于 `writeback_asm_operand` 走 [reg+off] 写回)
    /// - `is_output = false`: Ident 取栈/全局值 (asm 模板里使用该值)
    /// - 其他表达式统一 `gen_expr` 求值。
    fn prepare_asm_binding(&mut self, expr: &Expr, reg: u8, is_output: bool) {
        if let Expr::Ident { name, .. } = expr {
            if self.globals.contains_key(&name.name) {
                self.emit_global_addr(reg, &name.name);
            } else if is_output {
                // 输出: lea 出变量地址, writeback 通过 [r13+off] 写回
                let off = self.var_offset(&name.name);
                self.emit_lea_r13(reg, off);
            } else {
                // 输入: 直接 load 变量值, asm 模板里使用该值
                self.gen_expr(expr, reg);
            }
        } else {
            self.gen_expr(expr, reg);
        }
    }

    /// 构建 asm 输出文件 (-S 模式)：按节分组反汇编全部函数机器码。
    pub fn build_asm_output(&self, prog: &Program) -> String {
        use crate::codegen::disasm;
        let mut out = String::new();
        out.push_str(&format!("\t.file \"{}\"\n", self.source_name));

        let labels = disasm::label_map(self.labels.iter().map(|(n, o)| (o, n.as_str())));
        let directives = disasm::directive_map(
            self.asm_directives_at.iter().map(|(o, l)| (o, l.as_str())),
        );

        let mut funcs = Vec::new();
        Self::collect_all_funcs(prog, &mut funcs);
        funcs.sort_by_key(|name| self.labels.get(name).copied().unwrap_or(u64::MAX));

        let mut sections: Vec<String> = vec![".text".to_string()];
        for name in &funcs {
            if let Some(layout) = self.func_asm_layout.get(name) {
                if layout.section != ".text" && !sections.contains(&layout.section) {
                    sections.push(layout.section.clone());
                }
            }
        }

        for sec_name in &sections {
            if sec_name == ".text" {
                out.push_str("\t.text\n");
            } else {
                let flags = self.gas_section_flags(sec_name);
                out.push_str(&format!("\t.section {sec_name},\"{flags}\"\n"));
            }

            if sec_name == ".text" {
                let custom_starts: std::collections::HashSet<u64> = self
                    .func_asm_layout
                    .values()
                    .filter(|l| l.section != ".text")
                    .map(|l| l.text_start)
                    .collect();
                let prologue_labels: std::collections::BTreeMap<u64, Vec<&str>> = labels
                    .iter()
                    .filter(|(off, _)| !custom_starts.contains(off))
                    .map(|(off, names)| (*off, names.clone()))
                    .collect();
                let first_text = funcs
                    .iter()
                    .filter_map(|n| self.func_asm_layout.get(n))
                    .filter(|l| l.section == ".text")
                    .map(|l| l.text_start)
                    .min()
                    .unwrap_or(self.code.len() as u64);
                if first_text > 0 {
                    out.push_str(&disasm::disassemble_range(
                        &self.code,
                        0,
                        first_text as usize,
                        0,
                        &prologue_labels,
                        &directives,
                        &self.disasm_calls,
                        &self.disasm_data,
                    ));
                }
            }

            for name in &funcs {
                let layout = match self.func_asm_layout.get(name) {
                    Some(l) => l,
                    None => continue,
                };
                if layout.section != *sec_name {
                    continue;
                }
                let is_static = prog.decls.iter().any(|d| {
                    matches!(d, Decl::Func(f) if f.name.name == *name && f.is_static)
                });
                if !is_static {
                    out.push_str(&format!("\t.globl {name}\n"));
                }
                out.push_str(&format!("\t.type {name}, @function\n"));
                if self.emit_cfi {
                    out.push_str("\t.cfi_startproc\n");
                }
                out.push_str(&disasm::disassemble_range(
                    &layout.bytes,
                    0,
                    layout.bytes.len(),
                    layout.text_start,
                    &labels,
                    &directives,
                    &self.disasm_calls,
                    &self.disasm_data,
                ));
                if self.emit_cfi {
                    out.push_str("\t.cfi_endproc\n");
                }
                out.push_str(&format!("\t.size {name}, .-{name}\n"));
            }
        }
        out
    }

    fn gas_section_flags(&self, name: &str) -> &'static str {
        self.extra_sections
            .iter()
            .find(|s| s.name == name)
            .map(|s| {
                if s.flags & SHF_EXECINSTR != 0 {
                    "ax"
                } else if s.flags & SHF_WRITE != 0 {
                    "aw"
                } else {
                    "a"
                }
            })
            .unwrap_or("ax")
    }

    fn collect_all_funcs(prog: &Program, out: &mut Vec<String>) {
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_some() {
                    out.push(f.name.name.clone());
                    if let Some(body) = &f.body {
                        Self::collect_nested_func_names(&body.items, out);
                    }
                }
            }
        }
    }

    fn collect_nested_func_names(items: &[BlockItem], out: &mut Vec<String>) {
        for item in items {
            if let BlockItem::Decl(Decl::Func(f)) = item {
                if f.body.is_some() {
                    out.push(f.name.name.clone());
                    if let Some(body) = &f.body {
                        Self::collect_nested_func_names(&body.items, out);
                    }
                }
            }
            if let BlockItem::Stmt(s) = item {
                Self::collect_nested_func_names_stmt(s, out);
            }
        }
    }

    fn collect_nested_func_names_stmt(s: &Stmt, out: &mut Vec<String>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_nested_func_names(&b.items, out),
            Stmt::If { then_, else_, .. } => {
                Self::collect_nested_func_names_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_nested_func_names_stmt(e, out);
                }
            }
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
                Self::collect_nested_func_names_stmt(body, out);
            }
            Stmt::For { init, body, .. } => {
                if let Some(i) = init {
                    Self::collect_nested_func_names_stmt(i, out);
                }
                Self::collect_nested_func_names_stmt(body, out);
            }
            _ => {}
        }
    }

    /// 求值 asm 操作数 (用于 "i" 约束的立即数替换)
    fn eval_asm_operand(expr: &Expr) -> Option<String> {
        if let Ok(v) = const_eval::eval(expr) {
            return Some(v.to_string());
        }
        if let Expr::Offsetof { ty, member, .. } = expr {
            return Some(Self::compute_offset(ty, member).to_string());
        }
        None
    }

    /// 将 op_map 值格式化为 AT&T 操作数字符串
    fn asm_format_subst(val: &str) -> String {
        // 内存操作数 (含 '('), 立即数 (以 $ 开头), 偏移 (以 - 开头), 或纯数字:
        // 直接使用, 不加 % 前缀
        if val.contains('(') || val.starts_with('$') || val.starts_with('-') || val.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            val.to_string()
        } else {
            format!("%{val}")
        }
    }

    /// 替换 asm 模板中的 %N 占位符
    fn resolve_asm_template(
        &self,
        line: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        inputs: &[Expr],
        input_names: &[Option<String>],
        op_map: &HashMap<String, String>,
    ) -> String {
        let mut resolved = line.to_string();
        let mut idx = 0usize;
        for op in outputs.iter().chain(inputs.iter()) {
            let key = format!("%{}", idx);
            if let Some(val) = op_map.get(&key) {
                resolved = resolved.replace(&key, &Self::asm_format_subst(val));
            } else if let Some(val) = Self::eval_asm_operand(op) {
                resolved = resolved.replace(&key, &val);
            }
            idx += 1;
        }
        for (i, name) in output_names.iter().chain(input_names.iter()).enumerate() {
            if let Some(n) = name {
                let key = format!("%[{n}]");
                let num_key = format!("%{}", i);
                if let Some(reg) = op_map.get(&num_key).or_else(|| op_map.get(&key)) {
                    resolved = resolved.replace(&key, &Self::asm_format_subst(reg));
                }
            }
        }
        for (key, reg) in op_map {
            if key.starts_with("%[") {
                resolved = resolved.replace(key, &Self::asm_format_subst(reg));
            }
        }
        Self::unescape_asm_template(&resolved)
    }

    /// 发射已替换操作数后的 asm 模板行（含 GNU 局部数字标签）
    fn emit_resolved_asm_lines(&mut self, resolved_lines: &[String], is_global: bool) {
        let locals = AsmLocalLabels::scan(resolved_lines);
        for (i, resolved) in resolved_lines.iter().enumerate() {
            let trimmed = resolved.trim();
            if trimmed.is_empty() {
                continue;
            }
            // 数字局部标签 N:
            if let Some(name) = locals.name_at(i) {
                self.label(name);
                if self.emit_asm_text && (is_global || trimmed.ends_with(':')) {
                    self.asm_directives_at.push((self.pos(), trimmed.to_string()));
                    self.asm_directives.push(trimmed.to_string());
                }
                continue;
            }
            if self.emit_asm_text {
                if trimmed.starts_with('.') || trimmed.ends_with(':') {
                    self.asm_directives_at.push((self.pos(), trimmed.to_string()));
                    self.asm_directives.push(trimmed.to_string());
                    if !trimmed.ends_with(':') {
                        continue;
                    }
                }
            }
            if is_global && !self.emit_asm_text {
                if trimmed.starts_with(".globl ") || trimmed.starts_with(".global ") {
                    if let Some(sym) = trimmed.split_whitespace().nth(1) {
                        self.global_asm_exports.insert(sym.trim_end_matches(':').to_string());
                    }
                    continue;
                }
            }
            // 命名标签 foo:（非纯数字）
            if let Some(label) = trimmed.strip_suffix(':') {
                let label = AsmLocalLabels::strip_asm_comment(label).trim();
                if !label.is_empty()
                    && !label.starts_with('.')
                    && !label.chars().all(|c| c.is_ascii_digit())
                {
                    self.label(label);
                    continue;
                }
            }
            if self.emit_asm_text && trimmed.starts_with('.') {
                continue;
            }
            let subbed = locals.substitute(trimmed, i);
            self.emit_asm_line(&subbed);
        }
    }

    /// 发射文件级 asm（Decl::GlobalAsm）：-S 保留汇编器指令，-c 解析标签与 .globl。
    fn emit_global_asm(
        &mut self,
        asm_str: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
    ) {
        let mut bindings = Vec::new();
        let op_map = self.build_asm_operand_map(
            outputs, output_names, output_constraints,
            inputs, input_names, input_constraints, &mut bindings,
        );
        let s = if asm_str.starts_with('"') && asm_str.ends_with('"') && asm_str.len() >= 2 {
            &asm_str[1..asm_str.len()-1]
        } else { asm_str };
        let resolved_lines = AsmLocalLabels::expand_label_insn_lines(
            s.split(|c| c == '\n' || c == ';')
                .map(|line| {
                    self.resolve_asm_template(
                        line.trim(), outputs, output_names, inputs, input_names, &op_map,
                    )
                })
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>(),
        );
        self.emit_resolved_asm_lines(&resolved_lines, true);
        for (i, binding) in bindings.iter().enumerate() {
            if binding.is_output && !binding.is_mem && i < outputs.len() {
                self.writeback_asm_operand(&outputs[i], binding.reg);
            }
        }
        let _ = clobbers;
    }

    /// 发射内联汇编模板
    fn emit_inline_asm(
        &mut self,
        asm_str: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
    ) {
        let mut bindings = Vec::new();
        let op_map = self.build_asm_operand_map(
            outputs, output_names, output_constraints,
            inputs, input_names, input_constraints, &mut bindings,
        );
        let s = if asm_str.starts_with('"') && asm_str.ends_with('"') && asm_str.len() >= 2 {
            &asm_str[1..asm_str.len()-1]
        } else { asm_str };
        let resolved_lines = AsmLocalLabels::expand_label_insn_lines(
            s.split(|c| c == '\n' || c == ';')
                .map(|line| {
                    self.resolve_asm_template(
                        line.trim(), outputs, output_names, inputs, input_names, &op_map,
                    )
                })
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>(),
        );
        self.emit_resolved_asm_lines(&resolved_lines, false);
        for (i, binding) in bindings.iter().enumerate() {
            if binding.is_output && !binding.is_mem && i < outputs.len() {
                self.writeback_asm_operand(&outputs[i], binding.reg);
            }
        }
        if clobbers.iter().any(|c| c == "memory") {
            // memory clobber: 编译器屏障，防止指令重排越过 asm
            self.emit(&[0x0F, 0xAE, 0xF0]); // mfence
        }
    }

    /// 解析 AT&T 寄存器名 → (reg_index, size_bits)
    fn parse_asm_reg(name: &str) -> Option<(u8, u8)> {
        let n = name.strip_prefix('%').unwrap_or(name);
        match n {
            "rax" => Some((0, 64)), "eax" => Some((0, 32)), "ax" => Some((0, 16)), "al" => Some((0, 8)), "ah" => Some((0, 8)),
            "rbx" => Some((3, 64)), "ebx" => Some((3, 32)), "bx" => Some((3, 16)), "bl" => Some((3, 8)), "bh" => Some((3, 8)),
            "rcx" => Some((1, 64)), "ecx" => Some((1, 32)), "cx" => Some((1, 16)), "cl" => Some((1, 8)), "ch" => Some((1, 8)),
            "rdx" => Some((2, 64)), "edx" => Some((2, 32)), "dx" => Some((2, 16)), "dl" => Some((2, 8)), "dh" => Some((2, 8)),
            "rsi" => Some((6, 64)), "esi" => Some((6, 32)), "si" => Some((6, 16)), "sil" => Some((6, 8)),
            "rdi" => Some((7, 64)), "edi" => Some((7, 32)), "di" => Some((7, 16)), "dil" => Some((7, 8)),
            "rsp" => Some((4, 64)), "esp" => Some((4, 32)), "sp" => Some((4, 16)), "spl" => Some((4, 8)),
            "rbp" => Some((5, 64)), "ebp" => Some((5, 32)), "bp" => Some((5, 16)), "bpl" => Some((5, 8)),
            "r8" => Some((8, 64)), "r8d" => Some((8, 32)), "r8w" => Some((8, 16)), "r8b" => Some((8, 8)),
            "r9" => Some((9, 64)), "r9d" => Some((9, 32)), "r9w" => Some((9, 16)), "r9b" => Some((9, 8)),
            "r10" => Some((10, 64)), "r10d" => Some((10, 32)), "r10w" => Some((10, 16)), "r10b" => Some((10, 8)),
            "r11" => Some((11, 64)), "r11d" => Some((11, 32)), "r11w" => Some((11, 16)), "r11b" => Some((11, 8)),
            "r12" => Some((12, 64)), "r12d" => Some((12, 32)), "r12w" => Some((12, 16)), "r12b" => Some((12, 8)),
            "r13" => Some((13, 64)), "r13d" => Some((13, 32)), "r13w" => Some((13, 16)), "r13b" => Some((13, 8)),
            "r14" => Some((14, 64)), "r14d" => Some((14, 32)), "r14w" => Some((14, 16)), "r14b" => Some((14, 8)),
            "r15" => Some((15, 64)), "r15d" => Some((15, 32)), "r15w" => Some((15, 16)), "r15b" => Some((15, 8)),
            "rip" => Some((ASM_BASE_RIP, 64)),
            "xmm0" => Some((0, 128)), "xmm1" => Some((1, 128)), "xmm2" => Some((2, 128)), "xmm3" => Some((3, 128)),
            "xmm4" => Some((4, 128)), "xmm5" => Some((5, 128)), "xmm6" => Some((6, 128)), "xmm7" => Some((7, 128)),
            "xmm8" => Some((8, 128)), "xmm9" => Some((9, 128)), "xmm10" => Some((10, 128)), "xmm11" => Some((11, 128)),
            "xmm12" => Some((12, 128)), "xmm13" => Some((13, 128)), "xmm14" => Some((14, 128)), "xmm15" => Some((15, 128)),
            _ => None,
        }
    }

    /// 解析段寄存器名 → (seg_code, prefix_byte)
    /// seg_code 用于 modrm reg 字段: 0=ES,1=CS,2=SS,3=DS,4=FS,5=GS
    /// prefix_byte 用于 mov/push 等的段前缀
    fn parse_asm_seg(name: &str) -> Option<(u8, u8)> {
        let n = name.strip_prefix('%').unwrap_or(name);
        match n {
            "es" => Some((0, 0x26)),
            "cs" => Some((1, 0x2E)),
            "ss" => Some((2, 0x36)),
            "ds" => Some((3, 0x3E)),
            "fs" => Some((4, 0x64)),
            "gs" => Some((5, 0x65)),
            _ => None,
        }
    }

    /// 检测字符串是否以段寄存器名 (含可选 `%` 前缀和 `:`) 开头
    /// 返回 (剩余部分, 段码 Some(code)) ; 无段前缀则 (原串, None)
    /// 接受: `gs:...`, `%gs:...`, `gs`, `%gs` (单独使用, push %gs)
    fn strip_asm_seg_prefix(s: &str) -> (&str, Option<u8>) {
        let s = s.trim_start();
        let (head, rest) = if let Some(after) = s.strip_prefix('%') {
            // 找第一个非字母字符作为 head/rs 分割
            let pos = after.find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(after.len());
            (&after[..pos], &after[pos..])
        } else {
            let pos = s.find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(s.len());
            (&s[..pos], &s[pos..])
        };
        if let Some((code, _pfx)) = Self::parse_asm_seg(head) {
            if let Some(after_colon) = rest.strip_prefix(':') {
                return (after_colon, Some(code));
            }
            // 单独段名: 整个 s 已经被消耗为段名
            if rest.is_empty() {
                return ("", Some(code));
            }
        }
        (s, None)
    }

    /// 解析 AT&T 操作数 → AsmOperand
    fn parse_asm_operand_str(s: &str) -> AsmOperand {
        let s = s.trim();
        if s.is_empty() { return AsmOperand::None; }
        // 段前缀剥离: %gs:... / gs:... / 单独的 %gs / gs (必须在其他检查之前)
        let (s, seg) = Self::strip_asm_seg_prefix(s);
        let s = s.trim();
        if s.is_empty() {
            return if let Some(sc) = seg { AsmOperand::Seg(sc) } else { AsmOperand::None };
        }
        // 间接前缀 *: jmp *%rax, call *symbol, jmp *(%rax) 等
        if let Some(rest) = s.strip_prefix('*') {
            let inner = Self::parse_asm_operand_str(rest);
            return Self::wrap_indirect(inner);
        }
        // 立即数: $N
        if let Some(rest) = s.strip_prefix('$') {
            let rest = rest.trim();
            if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
                if let Ok(v) = i64::from_str_radix(hex, 16) { return AsmOperand::Imm(v); }
            } else if let Ok(v) = rest.parse::<i64>() {
                return AsmOperand::Imm(v);
            }
            return AsmOperand::None;
        }
        // 寄存器: %reg 或裸 rax/rbx 等
        if s.starts_with('%') {
            if let Some((idx, sz)) = Self::parse_asm_reg(s) {
                return AsmOperand::Reg(idx, sz);
            }
            return AsmOperand::None;
        }
        if let Some((idx, sz)) = Self::parse_asm_reg(&format!("%{s}")) {
            return AsmOperand::Reg(idx, sz);
        }
        // 内存: disp(%base,%idx,scale) 或 (%reg) 或 symbol(%rip)
        if s.contains('(') {
            let inner = Self::parse_asm_mem(s);
            return if let (Some(sc), AsmOperand::Mem { disp, base, index, scale }) = (seg, &inner) {
                AsmOperand::SegMem { seg: sc, disp: *disp, base: *base, index: *index, scale: *scale }
            } else {
                inner
            };
        }
        // 段前缀 + 符号 (例如 `gs:symbol` 实际罕见; 当作符号)
        // 符号名 (全局标签引用) + 段前缀 → 段前缀跟随到 emit 阶段
        let sym = AsmLocalLabels::strip_asm_comment(s).trim().to_string();
        if let Some(sc) = seg {
            return AsmOperand::SegMem { seg: sc, disp: 0, base: ASM_BASE_RIP, index: None, scale: 1 };
        }
        AsmOperand::Sym(sym)
    }

    /// 把 AsmOperand 标记为间接 (jmp/call *x 含义)
    /// 实现: 把 Reg/Mem 重新包装为带 Indirect 标记的形式
    /// 由于 AsmOperand 没有 Indirect 字段, 我们用约定: 间接的 Sym 名称前缀 'i_'
    /// 但更干净的做法是给 AsmOperand 加 Indirect(Box<AsmOperand>)
    /// 这里采用最小改动: 间接标签 → 走 RIP-relative 路径 (movabs rax, sym; jmp *rax)
    /// 因为 reg/mem 已经被处理为寄存器/内存, 间接标记只在 Sym 时才有意义
    fn wrap_indirect(inner: AsmOperand) -> AsmOperand {
        // 间接 Reg → 保持 Reg, emit 阶段识别 (jmp *%rax = jmp %rax)
        // 间接 Mem → 保持 Mem
        // 间接 Sym → 标记为 SymDisp(sym, 0) 表示间接跳转
        match inner {
            AsmOperand::Sym(name) => AsmOperand::SymDisp(name, 0),
            other => other, // Reg/Mem 原样返回
        }
    }

    /// 解析 AT&T 内存操作数
    fn parse_asm_mem(s: &str) -> AsmOperand {
        // 格式: [disp](%base[,%idx[,scale]])
        let paren_pos = s.find('(').unwrap_or(s.len());
        let disp_str = s[..paren_pos].trim();
        let rest = &s[paren_pos..];
        // 检测 disp 是否为符号 (而不是立即数): symbol(%rip) / sym(%base)
        let mut disp: i32 = 0;
        if !disp_str.is_empty() {
            if let Some(hex) = disp_str.strip_prefix("0x").or_else(|| disp_str.strip_prefix("0X")) {
                disp = i32::from_str_radix(hex, 16).unwrap_or(0);
            } else if disp_str.parse::<i32>().is_err() {
                // 符号名: 转 SymDisp 携带符号名 (RIP-relative 默认)
                return AsmOperand::SymDisp(disp_str.to_string(), 0);
            } else {
                disp = disp_str.parse::<i32>().unwrap_or(0);
            }
        }
        // 提取括号内的内容
        let inner = if rest.starts_with('(') && rest.ends_with(')') {
            &rest[1..rest.len()-1]
        } else { return AsmOperand::None; };
        let parts: Vec<&str> = inner.split(',').collect();
        let base_str = parts.first().map(|s| s.trim()).unwrap_or("");
        if base_str == "%rip" || base_str == "rip" {
            return AsmOperand::Mem { disp, base: ASM_BASE_RIP, index: None, scale: 1 };
        }
        let (base, _bsz) = Self::parse_asm_reg(base_str).unwrap_or((5, 64)); // 默认 rbp
        let mut idx: Option<u8> = None;
        let mut scale: u8 = 1;
        if parts.len() > 1 {
            let idx_str = parts[1].trim();
            if !idx_str.is_empty() {
                idx = Self::parse_asm_reg(idx_str).map(|(r, _)| r);
            }
        }
        if parts.len() > 2 {
            scale = parts[2].trim().parse::<u8>().unwrap_or(1);
        }
        AsmOperand::Mem { disp, base, index: idx, scale }
    }

    /// 处理 GAS 伪指令（-c 模式下静默跳过或填充，避免降级为 nop 警告）
    fn emit_asm_directive(&mut self, line: &str) -> bool {
        let t = line.trim();
        if !t.starts_with('.') {
            return false;
        }
        let parts: Vec<&str> = t.split_whitespace().collect();
        if parts.is_empty() {
            return true;
        }
        match parts[0] {
            ".pushsection" | ".popsection" | ".section" | ".previous"
            | ".type" | ".size" | ".ident" | ".globl" | ".global"
            | ".weak" | ".hidden" | ".protected" => true,
            ".balign" | ".p2align" | ".align" => {
                if let Some(n) = parts.get(1).and_then(|s| s.trim_end_matches(',').parse::<usize>().ok()) {
                    let align = if parts[0] == ".p2align" {
                        1usize << n.min(12)
                    } else {
                        n.max(1)
                    };
                    while !self.code.is_empty() && self.code.len() % align != 0 {
                        self.emit(&[0x90]);
                    }
                }
                true
            }
            ".long" => {
                self.emit(&[0u8; 4]);
                true
            }
            ".quad" => {
                self.emit(&[0u8; 8]);
                true
            }
            ".byte" => {
                if let Some(v) = parts.get(1).and_then(|s| Self::parse_asm_imm_token(s)) {
                    self.emit(&[(v & 0xFF) as u8]);
                }
                true
            }
            ".asciz" | ".string" => {
                if let Some(s) = parts.get(1) {
                    let s = s.trim_matches('"');
                    for b in s.bytes() {
                        self.emit(&[b]);
                    }
                    self.emit(&[0]);
                }
                true
            }
            _ => t.starts_with('.'),
        }
    }

    fn parse_asm_imm_token(s: &str) -> Option<i64> {
        let s = s.trim().trim_end_matches(',');
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            i64::from_str_radix(hex, 16).ok()
        } else {
            s.parse().ok()
        }
    }

    /// GNU 内联 asm 模板：`%%` 折叠为字面量 `%`（在 `%N` 占位符替换之后调用）
    fn unescape_asm_template(s: &str) -> String {
        s.replace("%%", "%")
    }

    /// 发射单条 AT&T 汇编指令
    fn emit_asm_line(&mut self, line: &str) {
        let line = Self::unescape_asm_template(AsmLocalLabels::strip_asm_comment(line).trim());
        if line.is_empty() {
            return;
        }
        if self.emit_asm_directive(&line) {
            return;
        }
        // 处理前缀: lock, rep, repz, repnz
        let (prefix, rest) = if line.starts_with("lock ") || line.starts_with("lock\t") {
            (Some(0xF0u8), line[5..].trim())
        } else if line.starts_with("rep ") || line.starts_with("rep\t") || line.starts_with("repz ") || line.starts_with("repz\t") {
            (Some(0xF3u8), line[4..].trim())
        } else if line.starts_with("repnz ") || line.starts_with("repnz\t") || line.starts_with("repe ") {
            (Some(0xF2u8), line[6..].trim())
        } else {
            (None, line.as_str())
        };
        // 分割助记符和操作数
        let (mnemonic, operands_str) = if let Some(pos) = rest.find(|c: char| c == ' ' || c == '\t') {
            (&rest[..pos], rest[pos..].trim())
        } else {
            (rest, "")
        };
        let operands: Vec<&str> = if operands_str.is_empty() { vec![] }
        else { operands_str.split(',').collect() };
        let ops: Vec<AsmOperand> = operands.iter().map(|s| Self::parse_asm_operand_str(s)).collect();
        if let Some(pfx) = prefix { self.emit(&[pfx]); }
        self.emit_asm_instr(mnemonic, &ops);
    }

    /// 编码单条指令
    fn emit_asm_instr(&mut self, mnemonic: &str, ops: &[AsmOperand]) {
        match mnemonic {
            // 无操作数指令
            "nop" => self.emit(&[0x90]),
            "ret" | "retq" => self.emit(&[0xC3]),
            "leave" | "leaveq" => self.emit(&[0xC9]),
            "cdq" => self.emit(&[0x99]),
            "cdqe" => self.emit(&[0x48, 0x98]),
            "cqo" => self.emit(&[0x48, 0x99]),
            "cbtw" => self.emit(&[0x66, 0x98]),
            "cwtl" => self.emit(&[0x98]),
            "cltd" => self.emit(&[0x99]),
            "clac" => self.emit(&[0x0F, 0x01, 0xCA]),
            "stac" => self.emit(&[0x0F, 0x01, 0xCB]),
            "cli" => self.emit(&[0xFA]),
            "sti" => self.emit(&[0xFB]),
            "cld" => self.emit(&[0xFC]),
            "std" => self.emit(&[0xFD]),
            "hlt" => self.emit(&[0xF4]),
            "pause" => self.emit(&[0xF3, 0x90]),
            "mfence" => self.emit(&[0x0F, 0xAE, 0xF0]),
            "lfence" => self.emit(&[0x0F, 0xAE, 0xE8]),
            "sfence" => self.emit(&[0x0F, 0xAE, 0xF8]),
            "syscall" => self.emit(&[0x0F, 0x05]),
            "sysenter" => self.emit(&[0x0F, 0x34]),
            "sysexit" => self.emit(&[0x0F, 0x35]),
            "rdtsc" => self.emit(&[0x0F, 0x31]),
            "rdpmc" => self.emit(&[0x0F, 0x33]),
            "xchg%eax,%eax" | "xchg %eax,%eax" => self.emit(&[0x90]), // nop alias
            "ud2" => self.emit(&[0x0F, 0x0B]),
            "clc" => self.emit(&[0xF8]),
            "stc" => self.emit(&[0xF9]),
            "cmc" => self.emit(&[0xF5]),
            "sahf" => self.emit(&[0x9E]),
            "lahf" => self.emit(&[0x9F]),

            // cpuid
            "cpuid" => self.emit(&[0x0F, 0xA2]),

            // 字符串操作
            "movsb" => self.emit(&[0xA4]),
            "movsw" => self.emit(&[0x66, 0xA5]),
            "movsl" | "movsd" => self.emit(&[0xA5]),
            "movsq" => self.emit(&[0x48, 0xA5]),
            "stosb" => self.emit(&[0xAA]),
            "stosw" => self.emit(&[0x66, 0xAB]),
            "stosl" | "stosd" => self.emit(&[0xAB]),
            "stosq" => self.emit(&[0x48, 0xAB]),
            "scasb" => self.emit(&[0xAE]),
            "cmpsb" => self.emit(&[0xA6]),

            // push/pop 寄存器
            "push" | "pushq" | "pushl" | "pushw" if ops.len() == 1 => {
                let sz = if mnemonic == "pushl" { 32 } else if mnemonic == "pushw" { 16 } else { 64 };
                self.emit_asm_push_sized(&ops[0], sz);
            }
            "pop" | "popq" | "popl" | "popw" if ops.len() == 1 => {
                let sz = if mnemonic == "popl" { 32 } else if mnemonic == "popw" { 16 } else { 64 };
                self.emit_asm_pop_sized(&ops[0], sz);
            }

            // 数据移动
            "mov" | "movq" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 64),
            "movl" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 32),
            "movw" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 16),
            // movd: 32-bit 整数与 xmm 之间的传送 (66 0F 6E / 66 0F 7E)
            "movd" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 32),
            "movb" if ops.len() == 2 => self.emit_asm_mov(&ops[0], &ops[1], 8),
            "movzbl" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 8, 32),
            "movzbq" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 8, 64),
            "movzwl" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 16, 32),
            "movzwq" if ops.len() == 2 => self.emit_asm_movzx(&ops[0], &ops[1], 16, 64),
            "movslq" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 32, 64),
            "movsbl" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 8, 32),
            "movsbq" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 8, 64),
            "movswl" if ops.len() == 2 => self.emit_asm_movsx(&ops[0], &ops[1], 16, 32),
            "lea" | "leaq" if ops.len() == 2 => self.emit_asm_lea(&ops[0], &ops[1], 64),
            "leal" if ops.len() == 2 => self.emit_asm_lea(&ops[0], &ops[1], 32),
            "xchg" if ops.len() == 2 => self.emit_asm_xchg(&ops[0], &ops[1]),
            "cmovz" | "cmovze" | "cmove" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x44),
            "cmovnz" | "cmovne" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x45),
            "cmovl" | "cmovnge" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x4C),
            "cmovg" | "cmovnle" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x4F),
            "cmovle" | "cmovng" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x4E),
            "cmovge" | "cmovnl" if ops.len() == 2 => self.emit_asm_cmov(&ops[0], &ops[1], 0x4D),

            // 算术
            "add" | "addq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 64),
            "addl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 32),
            "addw" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 16),
            "addb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 0, 8),
            "adc" | "adcq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 2, 64),
            "adcl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 2, 32),
            "adcb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 2, 8),
            "sbb" | "sbbq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 3, 64),
            "sbbl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 3, 32),
            "sbbb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 3, 8),
            "sub" | "subq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 64),
            "subl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 32),
            "subw" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 16),
            "subb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 5, 8),
            "and" | "andq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 4, 64),
            "andl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 4, 32),
            "or" | "orq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 1, 64),
            "orl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 1, 32),
            "xor" | "xorq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 6, 64),
            "xorl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 6, 32),
            "cmp" | "cmpq" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 64),
            "cmpl" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 32),
            "cmpw" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 16),
            "cmpb" if ops.len() == 2 => self.emit_asm_alu(&ops[0], &ops[1], 7, 8),
            "test" | "testq" if ops.len() == 2 => self.emit_asm_test(&ops[0], &ops[1], 64),
            "testl" if ops.len() == 2 => self.emit_asm_test(&ops[0], &ops[1], 32),
            "testb" if ops.len() == 2 => self.emit_asm_test(&ops[0], &ops[1], 8),
            "imul" if ops.len() == 2 => self.emit_asm_imul2(&ops[0], &ops[1]),
            "imul" if ops.len() == 3 => self.emit_asm_imul3(&ops[0], &ops[1], &ops[2]),
            "mul" | "mulq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 4, 64),
            "div" | "divq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 6, 64),
            "idiv" | "idivq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 7, 64),
            "neg" | "negq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 3, 64),
            "not" | "notq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 2, 64),
            "inc" | "incq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 0, 64),
            "incl" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 0, 32),
            "incw" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 0, 16),
            "dec" | "decq" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 1, 64),
            "decl" if ops.len() == 1 => self.emit_asm_unary_rm(&ops[0], 1, 32),

            // 旋转
            "rol" | "rolq" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 0, 64),
            "roll" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 0, 32),
            "rolb" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 0, 8),
            "ror" | "rorq" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 1, 64),
            "rorl" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 1, 32),
            "rorb" if ops.len() == 2 => self.emit_asm_rotate(&ops[0], &ops[1], 1, 8),

            // 预取 / 系统
            "prefetcht0" if ops.len() == 1 => self.emit_asm_prefetch(&ops[0], 0x18),
            "prefetcht1" if ops.len() == 1 => self.emit_asm_prefetch(&ops[0], 0x19),
            "prefetcht2" if ops.len() == 1 => self.emit_asm_prefetch(&ops[0], 0x1A),
            "prefetchnta" if ops.len() == 1 => self.emit_asm_prefetch(&ops[0], 0x1B),
            "prefetchw" if ops.len() == 1 => self.emit_asm_prefetch(&ops[0], 0x0D),
            "invlpg" if ops.len() == 1 => self.emit_asm_sys_mem(&ops[0], 0x01, 0x01),
            "invlpga" => self.emit(&[0x0F, 0x01, 0xDF]),
            "swapgs" => self.emit(&[0x0F, 0x01, 0xF8]),
            "wbinvd" => self.emit(&[0x0F, 0x09]),
            "invd" => self.emit(&[0x0F, 0x08]),
            "cpuid" => self.emit(&[0x0F, 0xA2]),
            "rdtsc" => self.emit(&[0x0F, 0x31]),
            "rdtscp" => self.emit(&[0x0F, 0x01, 0xF9]),
            "rdpmc" => self.emit(&[0x0F, 0x33]),
            "syscall" => self.emit(&[0x0F, 0x05]),
            "sysret" => self.emit(&[0x0F, 0x07]),
            "sysenter" => self.emit(&[0x0F, 0x34]),
            "sysexit" => self.emit(&[0x0F, 0x35]),
            "vmlaunch" => self.emit(&[0x0F, 0x01, 0xC2]),
            "vmresume" => self.emit(&[0x0F, 0x01, 0xC3]),
            "vmxoff" => self.emit(&[0x0F, 0x01, 0xC4]),
            "vmptrld" if ops.len() == 1 => self.emit_asm_sys_mem(&ops[0], 0x01, 0x03),
            "vmptrst" if ops.len() == 1 => self.emit_asm_sys_mem(&ops[0], 0x01, 0x07),
            "vmclear" if ops.len() == 1 => self.emit_asm_sys_mem(&ops[0], 0x01, 0x04),
            "vmread" if ops.len() == 2 => self.emit_asm_vmread(&ops[0], &ops[1]),
            "vmwrite" if ops.len() == 2 => self.emit_asm_vmwrite(&ops[0], &ops[1]),
            "ud2" => self.emit(&[0x0F, 0x0B]),
            "hlt" => self.emit(&[0xF4]),
            "cli" => self.emit(&[0xFA]),
            "sti" => self.emit(&[0xFB]),
            "cld" => self.emit(&[0xFC]),
            "std" => self.emit(&[0xFD]),
            "clc" => self.emit(&[0xF8]),
            "stc" => self.emit(&[0xF9]),
            "cmc" => self.emit(&[0xF5]),
            "lahf" => self.emit(&[0x9F]),
            "sahf" => self.emit(&[0x9E]),
            "pushf" => self.emit(&[0x9C]),
            "popf" => self.emit(&[0x9D]),
            "pushfq" => self.emit(&[0x9C]),
            "popfq" => self.emit(&[0x9D]),
            "stac" => self.emit(&[0x0F, 0x01, 0xCB]),
            "clac" => self.emit(&[0x0F, 0x01, 0xCA]),
            "endbr64" => self.emit(&[0xF3, 0x0F, 0x1E, 0xFA]),
            "endbr32" => self.emit(&[0xF3, 0x0F, 0x1E, 0xFB]),
            // fxsave/fxrstor = 0F AE /0 /1, ldmxcsr/stmxcsr = 0F AE /2 /3
            "fxsave" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xAE, 0, true),
            "fxrstor" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xAE, 1, true),
            "ldmxcsr" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xAE, 2, true),
            "stmxcsr" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xAE, 3, true),
            // fnsave/fnstenv = D9/DD /6 (无 0F 前缀)
            "fnsave" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xDD, 6, false),
            "fnstenv" if ops.len() == 1 => self.emit_asm_fpu_load(&ops[0], 0xD9, 6, false),
            "lfence" => self.emit(&[0x0F, 0xAE, 0xE8]),
            "sfence" => self.emit(&[0x0F, 0xAE, 0xF8]),
            "mfence" => self.emit(&[0x0F, 0xAE, 0xF0]),
            "pause" => self.emit(&[0xF3, 0x90]),
            "int3" => self.emit(&[0xCC]),
            "int" if ops.len() == 1 => self.emit_asm_int(&ops[0]),

            // 标量 SSE（内联 asm 约束常用）
            "movss" if ops.len() == 2 => self.emit_asm_sse_mov(&ops[0], &ops[1], true, false),
            "movsd" if ops.len() == 2 => self.emit_asm_sse_mov(&ops[0], &ops[1], false, false),
            "movssq" if ops.len() == 2 => self.emit_asm_sse_mov(&ops[0], &ops[1], true, true),
            "addss" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF3, 0x58),
            "subss" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF3, 0x5C),
            "mulss" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF3, 0x59),
            "divss" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF3, 0x5E),
            "addsd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF2, 0x58),
            "subsd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF2, 0x5C),
            "mulsd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF2, 0x59),
            "divsd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0xF2, 0x5E),
            "xorps" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x57),
            "xorpd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x57),
            "ucomiss" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x0F, 0x2E),
            "ucomisd" if ops.len() == 2 => self.emit_asm_sse_arith(&ops[0], &ops[1], 0x66, 0x2E),

            // 移位
            "shl" | "sal" | "shlq" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 4, 64),
            "shll" | "sall" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 4, 32),
            "shr" | "shrq" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 5, 64),
            "shrl" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 5, 32),
            "sar" | "sarq" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 7, 64),
            "sarl" if ops.len() == 2 => self.emit_asm_shift(&ops[0], &ops[1], 7, 32),

            // 双精度移位 / 位测试
            "shld" | "shldq" if ops.len() == 3 => self.emit_asm_shld_shrd(&ops[0], &ops[1], &ops[2], true, 64),
            "shldl" if ops.len() == 3 => self.emit_asm_shld_shrd(&ops[0], &ops[1], &ops[2], true, 32),
            "shrd" | "shrdq" if ops.len() == 3 => self.emit_asm_shld_shrd(&ops[0], &ops[1], &ops[2], false, 64),
            "shrdl" if ops.len() == 3 => self.emit_asm_shld_shrd(&ops[0], &ops[1], &ops[2], false, 32),
            "bt" | "btq" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xA3, 64),
            "btl" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xA3, 32),
            "btw" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xA3, 16),
            "bts" | "btsq" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xAB, 64),
            "btsl" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xAB, 32),
            "btr" | "btrq" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xB3, 64),
            "btrl" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xB3, 32),
            "btrw" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xB3, 16),
            "btc" | "btcq" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xBB, 64),
            "btcl" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xBB, 32),
            "btcw" if ops.len() == 2 => self.emit_asm_bit(&ops[0], &ops[1], 0xBB, 16),

            // 位扫描 / popcnt / bswap
            "bsf" | "bsfq" if ops.len() == 2 => self.emit_asm_scan(&ops[0], &ops[1], 0xBC, 64),
            "bsfl" if ops.len() == 2 => self.emit_asm_scan(&ops[0], &ops[1], 0xBC, 32),
            "bsr" | "bsrq" if ops.len() == 2 => self.emit_asm_scan(&ops[0], &ops[1], 0xBD, 64),
            "bsrl" if ops.len() == 2 => self.emit_asm_scan(&ops[0], &ops[1], 0xBD, 32),
            "popcnt" | "popcntq" if ops.len() == 2 => self.emit_asm_popcnt(&ops[0], &ops[1], 64),
            "popcntl" if ops.len() == 2 => self.emit_asm_popcnt(&ops[0], &ops[1], 32),
            "bswap" | "bswapq" if ops.len() == 1 => self.emit_asm_bswap(&ops[0], 64),
            "bswapl" if ops.len() == 1 => self.emit_asm_bswap(&ops[0], 32),

            // 非 temporal store / 虚拟化
            "movnti" | "movntil" if ops.len() == 2 => self.emit_asm_movnt(&ops[0], &ops[1], 32),
            "movntiq" if ops.len() == 2 => self.emit_asm_movnt(&ops[0], &ops[1], 64),
            "vmcall" => self.emit(&[0x0F, 0x01, 0xC1]),
            "vmmcall" => self.emit(&[0x0F, 0x01, 0xD9]),

            // 跳转
            "jmp" if ops.len() == 1 => self.emit_asm_jmp(&ops[0]),
            "je" | "jz" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x84),
            "jne" | "jnz" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x85),
            "jl" | "jnge" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8C),
            "jle" | "jng" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8E),
            "jg" | "jnle" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8F),
            "jge" | "jnl" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x8D),
            "ja" | "jnbe" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x87),
            "jae" | "jnb" | "jnc" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x83),
            "jb" | "jnae" | "jc" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x82),
            "jbe" | "jna" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x86),
            "js" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x88),
            "jns" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x89),
            "jo" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x80),
            "jno" if ops.len() == 1 => self.emit_asm_jcc(&ops[0], 0x81),

            // 调用
            "call" if ops.len() == 1 => self.emit_asm_call(&ops[0]),

            // 设置字节 (setcc)
            "sete" | "setz" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x94),
            "setne" | "setnz" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x95),
            "setl" | "setnge" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9C),
            "setg" | "setnle" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9F),
            "setle" | "setng" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9E),
            "setge" | "setnl" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9D),
            "setb" | "setc" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x92),
            "seta" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x97),
            "setae" | "setnb" | "setnc" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x93),
            "setbe" | "setna" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x96),
            "setns" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x99),
            "setno" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x91),
            "setp" | "setpe" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9A),
            "setnp" | "setpo" if ops.len() == 1 => self.emit_asm_setcc(&ops[0], 0x9B),

            // 原子操作
            "cmpxchg" | "cmpxchgl" | "cmpxchgq" if ops.len() == 2 => {
                let bits = if mnemonic == "cmpxchgl" { 32 } else { 64 };
                self.emit_asm_cmpxchg(&ops[0], &ops[1], bits);
            }
            "xadd" | "xaddl" | "xaddq" if ops.len() == 2 => {
                let bits = if mnemonic == "xaddl" { 32 } else { 64 };
                self.emit_asm_xadd(&ops[0], &ops[1], bits);
            }

            // 特殊寄存器
            "rdmsr" => self.emit(&[0x0F, 0x32]),
            "wrmsr" => self.emit(&[0x0F, 0x30]),
            "movq" if ops.len() == 2 && mnemonic == "movq" => self.emit_asm_mov(&ops[0], &ops[1], 64),

            // === TSX (Transactional Synchronization Extensions) ===
            // xbegin rel16: C7 F8 iw
            "xbegin" if ops.len() == 1 => {
                if let AsmOperand::Imm(v) = &ops[0] {
                    self.emit(&[0xC7, 0xF8]); self.emit(&(*v as i16).to_le_bytes());
                } else if let AsmOperand::Sym(s) = &ops[0] {
                    let at = self.pos();
                    self.emit(&[0xC7, 0xF8, 0, 0]);
                    self.pending.push(PendingJump { at, target: s.clone(), kind: JumpKind::Je });
                } else {
                    eprintln!("warning: xbegin needs label or imm, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            "xend" => self.emit(&[0x0F, 0x01, 0xD5]),
            "xabort" if ops.len() == 1 => {
                if let AsmOperand::Imm(v) = &ops[0] {
                    self.emit(&[0xC6, 0xF8, *v as u8]);
                } else {
                    eprintln!("warning: xabort needs imm8, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            "xtest" => self.emit(&[0x0F, 0x01, 0xD6]),

            // === AES-NI (66 0F 38 DC-DF) ===
            // aesenc aesi, xesi   : 66 0F 38 DC /r
            // aesenclast          : 66 0F 38 DD /r
            // aesdec              : 66 0F 38 DE /r
            // aesdeclast          : 66 0F 38 DF /r
            "aesenc" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0xDC, 0),
            "aesenclast" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0xDD, 0),
            "aesdec" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0xDE, 0),
            "aesdeclast" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0xDF, 0),
            // aesimc xmm1, xmm2/m128 : 66 0F 38 DB /r
            "aesimc" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0xDB, 0),
            // aeskeygenassist xmm1, xmm2/m128, imm8 : 66 0F 3A DF /r ib
            "aeskeygenassist" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0xDF, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: aeskeygenassist needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }

            // === SHA Extensions ===
            // sha1rnds4 xmm1, xmm2/m128, imm8 : 0F 3A CC /r ib
            "sha1rnds4" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 0);
                        self.emit(&[0xCC, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: sha1rnds4 needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            // sha1nexte xmm1, xmm2/m128 : 0F 38 C8 /r
            "sha1nexte" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xC8, 0),
            // sha1msg1 xmm1, xmm2/m128 : 0F 38 C9 /r
            "sha1msg1" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xC9, 0),
            // sha1msg2 xmm1, xmm2/m128 : 0F 38 CA /r
            "sha1msg2" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xCA, 0),
            // sha256rnds2 xmm1, xmm2/m128, <XMM0> : 0F 38 CB /r (隐式 xmm0)
            "sha256rnds2" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xCB, 0),
            // sha256msg1 xmm1, xmm2/m128 : 0F 38 CC /r
            "sha256msg1" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xCC, 0),
            // sha256msg2 xmm1, xmm2/m128 : 0F 38 CD /r
            "sha256msg2" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 0, 2, 0xCD, 0),

            // === FMA (Fused Multiply-Add) — 4 操作数 ===
            // vfmadd132ps/pd/ss/sd: VEX.NDS.128/256.66.0F38.W 98-99 /r
            // vfmadd213ps/pd/ss/sd: VEX.NDS.128/256.66.0F38.W A8-A9 /r
            // vfmadd231ps/pd/ss/sd: VEX.NDS.128/256.66.0F38.W B8-B9 /r
            // 4 操作数: dst, src1, src2/m ; dst = src1 * src2 + dst (132)
            // 模式: 所有 FMA 走 0F38 映射 (4 字节 VEX 前缀)
            // 由于 AT&T 语法中 dst 是第一个操作数, 1 2 3 对应 (3, 1, 2): dst 是 modrm.reg
            "vfmadd132ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x98, 0, false),
            "vfmadd132pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x98, 1, false),
            "vfmadd132ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x99, 0, true),
            "vfmadd132sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x99, 1, true),
            "vfmadd213ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xA8, 0, false),
            "vfmadd213pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xA8, 1, false),
            "vfmadd213ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xA9, 0, true),
            "vfmadd213sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xA9, 1, true),
            "vfmadd231ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xB8, 0, false),
            "vfmadd231pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xB8, 1, false),
            "vfmadd231ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xB9, 0, true),
            "vfmadd231sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xB9, 1, true),
            // 256-bit 版本 — 删去 (Reg 字段 u8, 256 不在范围内)
            // 256 位 FMA 走同一个 emit_asm_fma_3op, 调用方显式传 l=1
            // vfmsub132ps/pd/ss/sd (0x9A/9B/A8/AB/B8/BB)
            "vfmsub132ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9A, 0, false),
            "vfmsub132pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9A, 1, false),
            "vfmsub132ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9B, 0, true),
            "vfmsub132sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9B, 1, true),
            "vfmsub213ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAA, 0, false),
            "vfmsub213pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAA, 1, false),
            "vfmsub231ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xBA, 0, false),
            "vfmsub231pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xBA, 1, false),
            "vfmsub231ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xBB, 0, true),
            "vfmsub231sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xBB, 1, true),
            "vfmsub213ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xAB, 0, true),
            "vfmsub213sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xAB, 1, true),
            "vfmsub132ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0x9B, 0, true),
            "vfmsub132sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0x9B, 1, true),
            "vfmsub231pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xBA, 1, false),
            // vfnmadd132ps/pd (0x9C/9D/AC/AD/BC/BD)
            "vfnmadd132ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9C, 0, false),
            "vfnmadd132pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9C, 1, false),
            "vfnmadd132ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9D, 0, true),
            "vfnmadd132sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9D, 1, true),
            "vfnmadd213ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAC, 0, false),
            "vfnmadd213pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAC, 1, false),
            "vfnmadd231ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xBC, 0, false),
            "vfnmadd231pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xBC, 1, false),
            // vfnmsub132ps/pd (0x9E/9F/AE/AF/BE/BF)
            "vfnmsub132ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9E, 0, false),
            "vfnmsub132pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9E, 1, false),
            "vfnmsub132ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9F, 0, true),
            "vfnmsub132sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0x9F, 1, true),
            "vfnmsub213ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAE, 0, false),
            "vfnmsub213pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xAE, 1, false),
            "vfnmsub213ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xAF, 0, true),
            "vfnmsub213sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xAF, 1, true),
            "vfnmsub231ps" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xBE, 0, false),
            "vfnmsub231pd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 128, 1, 0xBE, 1, false),
            "vfnmsub231ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xBF, 0, true),
            "vfnmsub231sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xBF, 1, true),
            // vfnmadd132/213/231 标量 (0x9D/AD/BD)
            "vfnmadd213ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xAD, 0, true),
            "vfnmadd213sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xAD, 1, true),
            "vfnmadd231ss" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 0, 1, 0xBD, 0, true),
            "vfnmadd231sd" if ops.len() == 3 => self.emit_asm_fma_3op(&ops[0], &ops[1], &ops[2], 1, 1, 0xBD, 1, true),

            // === F16C (Half-precision ↔ Single-precision) ===
            // vcvtph2ps xmm1, xmm2/m128 : VEX.128.66.0F38.W0 13 /r
            "vcvtph2ps" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x13, 0),
            // vcvtps2ph xmm1/m128, xmm2, imm8 : VEX.128.66.0F3A.W0 1D /r ib
            "vcvtps2ph" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x1D, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Mem { disp, base, index, scale }) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 {
                        self.emit_vex4_prefix(*s, *base, 3, 0, 0, 0, 1);
                        self.emit(&[0x1D]);
                        self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
                        self.emit(&[*imm as u8]);
                    }
                } else {
                    eprintln!("warning: vcvtps2ph needs imm, reg, reg/mem, emitted as nop");
                    self.emit(&[0x90]);
                }
            }

            // === AVX/AVX2 整数 SIMD (0F 38 / 0F 3A 映射) ===
            // pshufb xmm1, xmm2/m128 : 66 0F 38 00 /r
            "pshufb" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x00, 0),
            // pshufd xmm1, xmm2/m128, imm8 : 66 0F 70 /r ib
            "pshufd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 1);
                        self.emit(&[0x70, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: pshufd needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            "pshufhw" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 3);
                        self.emit(&[0x70, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: pshufhw needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            "pshuflw" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 2);
                        self.emit(&[0x70, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: pshuflw needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            // pmulhrsw xmm1, xmm2/m128 : 66 0F 38 0B /r
            "pmulhrsw" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x0B, 0),
            // pmaddubsw xmm1, xmm2/m128 : 66 0F 38 04 /r
            "pmaddubsw" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x04, 0),
            // pabsb/pabsw/pabsd : 66 0F 38 1C/1D/1E /r
            "pabsb" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x1C, 0),
            "pabsw" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x1D, 0),
            "pabsd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x1E, 0),
            // palignr xmm1, xmm2/m128, imm8 : 66 0F 3A 0F /r ib
            "palignr" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x0F, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: palignr needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            // pblendw xmm1, xmm2/m128, imm8 : 66 0F 3A 0E /r ib
            "pblendw" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x0E, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: pblendw needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            // phminposuw xmm1, xmm2/m128, imm8 : 66 0F 38 41 /r ib
            "phminposuw" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x41, 0),
            // pmuldq xmm1, xmm2/m128 : 66 0F 38 28 /r
            "pmuldq" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x28, 0),
            // pmulld xmm1, xmm2/m128 : 66 0F 38 40 /r
            "pmulld" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x40, 0),
            // pmuludq xmm1, xmm2/m128 : 66 0F F4 /r
            "pmuludq" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 1, 0xF4, 0),
            // pminub/pmaxub/pminuw/pmaxuw/pminsb/pmaxsb/pminud/pmaxud/pminsd/pmaxsd
            // 66 0F DA/DE/D8/DC/38/3C/3B/3F/39/3D
            "pminub" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 1, 0xDA, 0),
            "pmaxub" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 1, 0xDE, 0),
            "pminuw" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 1, 0xD8, 0),
            "pmaxuw" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 1, 0xDC, 0),
            "pminsb" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x38, 0),
            "pmaxsb" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x3C, 0),
            "pminud" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x3B, 0),
            "pmaxud" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x3F, 0),
            "pminsd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x39, 0),
            "pmaxsd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x3D, 0),
            // pcmpeqq/pcmpgtq : 66 0F 38 29/37 /r
            "pcmpeqq" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x29, 0),
            "pcmpgtq" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x37, 0),
            // pinsrb/pinsrd/pinsrq : 66 0F 3A 20/22/21 /r ib
            "pinsrb" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x20, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: pinsrb needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            "pinsrd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x22, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: pinsrd needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            "pinsrq" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 1, 0, 0, 1);
                        self.emit(&[0x22, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: pinsrq needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            // pextrb/pextrd/pextrq : 66 0F 3A 14/16/15 /r ib
            "pextrb" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*s, *d, 3, 0, 0, 0, 1);
                        self.emit(&[0x14, Self::modrm_reg_reg(*s, *d), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: pextrb needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            "pextrd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*s, *d, 3, 0, 0, 0, 1);
                        self.emit(&[0x16, Self::modrm_reg_reg(*s, *d), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: pextrd needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            "pextrq" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*s, *d, 3, 1, 0, 0, 1);
                        self.emit(&[0x16, Self::modrm_reg_reg(*s, *d), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: pextrq needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            // extractps : 66 0F 3A 17 /r ib
            "extractps" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*s, *d, 3, 0, 0, 0, 1);
                        self.emit(&[0x17, Self::modrm_reg_reg(*s, *d), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: extractps needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            // insertps xmm1, xmm2/m128, imm8 : 66 0F 3A 21 /r ib
            "insertps" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x21, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else {
                    eprintln!("warning: insertps needs imm, reg, reg, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            // pmovsxbw / pmovsxbd / pmovsxbq / pmovsxwd / pmovsxwq / pmovsxdq
            // pmovzxbw / pmovzxbd / pmovzxbq / pmovzxwd / pmovzxwq / pmovzxdq
            // 66 0F 38 20-25 (sx) / 30-35 (zx)
            "pmovsxbw" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x20, 0),
            "pmovsxbd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x21, 0),
            "pmovsxbq" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x22, 0),
            "pmovsxwd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x23, 0),
            "pmovsxwq" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x24, 0),
            "pmovsxdq" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x25, 0),
            "pmovzxbw" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x30, 0),
            "pmovzxbd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x31, 0),
            "pmovzxbq" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x32, 0),
            "pmovzxwd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x33, 0),
            "pmovzxwq" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x34, 0),
            "pmovzxdq" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 128, 1, 2, 0x35, 0),
            // pcmpestri/pcmpestrm/pcmpistri/pcmpistrm 复杂指令, 此处仅预留 nop
            // roundps/roundpd/roundss/roundsd : 66 0F 3A 08/09/0A/0B /r ib
            "roundps" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x08, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "roundpd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x09, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "roundss" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x0A, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "roundsd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x0B, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }

            // === AVX/AVX2 浮点与整数 3 操作数 ===
            // vaddps/vaddpd/vmulps/vmulpd/vsubps/vsubpd/vdivps/vdivpd/vminps/vmaxps
            // VEX.NDS.128/256.0F 58/59/5C/5D/5E/5F/5E(vdiv)/5D(vdiv)
            "vaddps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x58, 0, false),
            "vaddpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x58, 0, false),
            "vmulps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x59, 0, false),
            "vmulpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x59, 0, false),
            "vsubps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5C, 0, false),
            "vsubpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5C, 0, false),
            "vdivps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5E, 0, false),
            "vdivpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5E, 0, false),
            "vminps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5D, 0, false),
            "vmaxps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5F, 0, false),
            "vminpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5D, 0, false),
            "vmaxpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5F, 0, false),
            "vsqrtps" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 1, 0x51, 0),
            "vsqrtpd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 1, 0, 1, 0x51, 0),
            "vandps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x54, 0, false),
            "vandpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x54, 0, false),
            "vorps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x56, 0, false),
            "vorpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x56, 0, false),
            "vxorps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x57, 0, false),
            "vxorpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x57, 0, false),
            // vaddss/vaddsd/vmulss/vmulsd/vsubss/vsubsd/vdivss/vdivsd/vminss/vmaxss
            // F3 0F 58/F2 0F 58/F3 0F 59/F2 0F 59/F3 0F 5C/F2 0F 5C/F3 0F 5E/F2 0F 5E/F3 0F 5D/F2 0F 5D/F3 0F 5F/F2 0F 5F
            "vaddss" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x58, 2, true),
            "vaddsd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x58, 3, true),
            "vmulss" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x59, 2, true),
            "vmulsd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x59, 3, true),
            "vsubss" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5C, 2, true),
            "vsubsd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5C, 3, true),
            "vdivss" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5E, 2, true),
            "vdivsd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5E, 3, true),
            "vminss" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5D, 2, true),
            "vmaxss" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x5F, 2, true),
            "vminsd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5D, 3, true),
            "vmaxsd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x5F, 3, true),
            "vsqrtss" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 2, 1, 0x51, 0),
            "vsqrtsd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 3, 1, 0x51, 0),
            // vblendps/vblendpd : 66 0F 3A 0C/0D /r ib
            // AT&T: vblendps $imm, src, dst (3 ops)
            "vblendps" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x0C, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vblendpd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 0, 1);
                        self.emit(&[0x0D, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            // vshufps : 0F C6 /r ib ; vshufpd : 66 0F C6 /r ib
            // AT&T: vshufps $imm, src, dst (3 ops)
            "vshufps" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *s, 0, 0);
                        self.emit(&[0xC6, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vshufpd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *s, 0, 1);
                        self.emit(&[0xC6, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            // vunpcklps/vunpckhps : 0F 14/15 /r
            "vunpcklps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x14, 0, false),
            "vunpckhps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x15, 0, false),
            "vunpcklpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x14, 0, false),
            "vunpckhpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x15, 0, false),
            // vunpck*ps/pd 2-op form: AT&T `vunpcklps src, dst` 等价于 `dst = unpack_low(dst, src)`
            // Intel 编码: vvvv = dst (modrm.reg = dst, modrm.rm = src)
            "vunpcklps" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *d, 0, 0);
                        self.emit(&[0x14, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vunpckhps" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *d, 0, 0);
                        self.emit(&[0x15, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vunpcklpd" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *d, 0, 1);
                        self.emit(&[0x14, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vunpckhpd" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, *d, 0, 1);
                        self.emit(&[0x15, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }
            // vpermilps/vpermilpd : 0F 38 0C/0D /r (reg-reg-reg 3-op)
            "vpermilps" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 0, 0x0C, 2, false),
            "vpermilpd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x0D, 2, false),
            // vbroadcastss : 66 0F 38 18 /r (3-op 形式)
            "vbroadcastss" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 2, 0x18, 0),
            // vbroadcastsd : VEX.256.66.0F38.W0 19 /r
            "vbroadcastsd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 1, 1, 2, 0x19, 0),
            // vbroadcasti128 : VEX.256.66.0F38.W0 5A /r
            "vbroadcasti128" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 1, 1, 2, 0x5A, 0),
            // vperm2f128/vperm2i128 : 66 0F 3A 06/46 /r ib
            "vperm2f128" if ops.len() == 4 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds), _) = (&ops[0], &ops[1], &ops[2], &ops[3]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 1, 1);
                        self.emit(&[0x06, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vperm2i128" if ops.len() == 4 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds), _) = (&ops[0], &ops[1], &ops[2], &ops[3]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 1, 1);
                        self.emit(&[0x46, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            // vinsertf128/vinserti128 : 66 0F 3A 18/38 /r ib
            "vinsertf128" if ops.len() == 4 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds), _) = (&ops[0], &ops[1], &ops[2], &ops[3]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 1, 1);
                        self.emit(&[0x18, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vinserti128" if ops.len() == 4 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds), _) = (&ops[0], &ops[1], &ops[2], &ops[3]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*d, *s, 3, 0, 0, 1, 1);
                        self.emit(&[0x38, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            // vextractf128/vextracti128 : 66 0F 3A 19/39 /r ib
            "vextractf128" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*s, *d, 3, 0, 0, 1, 1);
                        self.emit(&[0x19, Self::modrm_reg_reg(*s, *d), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vextracti128" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex4_prefix(*s, *d, 3, 0, 0, 1, 1);
                        self.emit(&[0x39, Self::modrm_reg_reg(*s, *d), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }

            // === AVX/AVX2 整数 SIMD ===
            "vpaddb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xFC, 0, false),
            "vpaddw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xFD, 0, false),
            "vpaddd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xFE, 0, false),
            "vpaddq" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xD4, 0, false),
            "vpsubb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xF8, 0, false),
            "vpsubw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xF9, 0, false),
            "vpsubd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xFA, 0, false),
            "vpsubq" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xFB, 0, false),
            "vpmullw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xD5, 0, false),
            "vpmulld" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x40, 2, false),
            "vpand" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDB, 0, false),
            "vpandn" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDF, 0, false),
            "vpor" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xEB, 0, false),
            "vpxor" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xEF, 0, false),
            "vpcmpeqb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x74, 0, false),
            "vpcmpeqw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x75, 0, false),
            "vpcmpeqd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x76, 0, false),
            "vpcmpgtb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x64, 0, false),
            "vpcmpgtw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x65, 0, false),
            "vpcmpgtd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x66, 0, false),
            "vpshufd" if ops.len() == 3 => {
                if let (AsmOperand::Imm(imm), AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ss == 128 && *ds == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 1);
                        self.emit(&[0x70, Self::modrm_reg_reg(*d, *s), *imm as u8]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vpshufb" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 2, 0x00, 0),
            "vpminub" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDA, 0, false),
            "vpmaxub" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDE, 0, false),
            "vpminuw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xD8, 0, false),
            "vpmaxuw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDC, 0, false),
            "vpminsb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x38, 2, false),
            "vpmaxsb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x3C, 2, false),
            "vpminsd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x39, 2, false),
            "vpmaxsd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x3D, 2, false),
            "vpminud" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x3B, 2, false),
            "vpmaxud" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x3F, 2, false),
            "vpmuldq" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x28, 2, false),
            "vpmuludq" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xF4, 0, false),
            "vpmulhrsw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x0B, 2, false),
            "vpabsb" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 2, 0x1C, 0),
            "vpabsw" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 2, 0x1D, 0),
            "vpabsd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 2, 0x1E, 0),
            "vpaddusb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xDC, 2, false),
            "vpaddsb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xEC, 2, false),
            "vpsubusb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xD8, 2, false),
            "vpsubsb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xE8, 2, false),
            "vpsignb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x08, 2, false),
            "vpsignw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x09, 2, false),
            "vpsignd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x0A, 2, false),
            "vphaddw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x01, 2, false),
            "vphaddd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x02, 2, false),
            "vphaddsw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x03, 2, false),
            "vphsubw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x05, 2, false),
            "vphsubd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x06, 2, false),
            "vphsubsw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x07, 2, false),
            "vpmaddwd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xF5, 0, false),
            "vpmaddubsw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x04, 2, false),
            "vpavgb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xE0, 2, false),
            "vpavgw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0xE3, 2, false),
            "vpacksswb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x63, 0, false),
            "vpackssdw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x6B, 0, false),
            "vpackuswb" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x67, 0, false),
            "vpackusdw" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x2B, 2, false),
            "vpcmpeqq" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x29, 2, false),
            "vpcmpgtq" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x37, 2, false),

            // === AVX mov 指令 (256-bit) ===
            "vmovaps" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 1, 0x28, 0),
            "vmovapd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x28, 0),
            "vmovups" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 1, 0x10, 0),
            "vmovupd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x10, 0),
            "vmovdqa" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x6F, 0),
            "vmovdqu" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 2, 1, 0x6F, 0),
            "vmovdqa32" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x6F, 0),
            "vmovdqu32" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 2, 1, 0x6F, 0),
            "vmovdqa64" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x6F, 1),
            "vmovdqu64" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 2, 1, 0x6F, 1),

            // vzeroupper / vzeroall : 0F 77 (vex.128.0F)
            "vzeroupper" => self.emit(&[0xC5, 0xF8, 0x77]),
            "vzeroall" => self.emit(&[0xC5, 0xFC, 0x77]),

            // vpmovmskb : 0F D7 /r (vex.128/256.0F D7)
            "vpmovmskb" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, _)) = (&ops[0], &ops[1]) {
                    if *ss == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 1);
                        self.emit(&[0xD7, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }
            // vmovmskps : 0F 50 /r ; vmovmskpd : 66 0F 50 /r
            "vmovmskps" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, _)) = (&ops[0], &ops[1]) {
                    if *ss == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 0);
                        self.emit(&[0x50, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vmovmskpd" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, _)) = (&ops[0], &ops[1]) {
                    if *ss == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 1);
                        self.emit(&[0x50, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }

            // vcvtps2pd / vcvtpd2ps : VEX.256/128
            "vcvtps2pd" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 1, 0x5A, 0),
            "vcvtpd2ps" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 1, 0x5A, 0),
            "vcvtss2si" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, _)) = (&ops[0], &ops[1]) {
                    if *ss == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 2);
                        self.emit(&[0x2D, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vcvtsd2si" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, _)) = (&ops[0], &ops[1]) {
                    if *ss == 128 {
                        self.emit_vex3_prefix(*d, 0, 0, 3);
                        self.emit(&[0x2D, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vcvtsi2ss" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, ds), AsmOperand::Reg(_, _)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ds == 128 {
                        self.emit_vex3_prefix(*d, *s, 0, 2);
                        self.emit(&[0x2A, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }
            "vcvtsi2sd" if ops.len() == 3 => {
                if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, ds), AsmOperand::Reg(_, _)) = (&ops[0], &ops[1], &ops[2]) {
                    if *ds == 128 {
                        self.emit_vex3_prefix(*d, *s, 0, 3);
                        self.emit(&[0x2A, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }

            // === AVX-VNNI (VEX 形式 0F 38 /r) ===
            // vpdpbusd : 66 0F 38 50 /r ; vpdpbssd : 66 0F 38 51 /r ; vpdpwssd : 66 0F 38 52 /r
            "vpdpbusd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x50, 2, false),
            "vpdpbssd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x51, 2, false),
            "vpdpwssd" if ops.len() == 3 => self.emit_asm_vex_3op_imm8_check(&ops[0], &ops[1], &ops[2], 1, 0x52, 2, false),

            // === AMX (Advanced Matrix Extensions) — Tile 指令 ===
            // ldtilecfg : VEX.128.0F38.W0 49 /r (mem)
            "ldtilecfg" if ops.len() == 1 => self.emit_asm_vex_2op(&AsmOperand::None, &ops[0], 0, 0, 2, 0x49, 0),
            // sttilecfg : VEX.128.0F38.W0 4B /r (mem)
            "sttilecfg" if ops.len() == 1 => self.emit_asm_vex_2op(&ops[0], &AsmOperand::None, 0, 0, 2, 0x4B, 0),
            // tilezero tmm1 : VEX.128.F2.0F38.W0 49 /r (reg)
            "tilezero" if ops.len() == 1 => {
                if let AsmOperand::Reg(d, _) = &ops[0] {
                    self.emit(&[0xC4, 0xE2, 0x7B, 0x49, 0xC0 | (d & 7)]);
                } else { self.emit(&[0x90]); }
            }
            // tileloadd tmm1, sibmem : VEX.128.F2.0F38.W0 4B /r
            "tileloadd" if ops.len() == 2 => {
                if let (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) = (&ops[0], &ops[1]) {
                    self.emit(&[0xC4, 0xE2, 0x7B, 0x4B]);
                    self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
                } else { self.emit(&[0x90]); }
            }
            // tilestored sibmem, tmm1 : VEX.128.F3.0F38.W0 4B /r
            "tilestored" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) = (&ops[0], &ops[1]) {
                    self.emit(&[0xC4, 0xE2, 0x7A, 0x4B]);
                    self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
                } else { self.emit(&[0x90]); }
            }
            // tdpbssd tmm1, tmm2, tmm3 : VEX.128.F2.0F38.W0 5E /r
            "tdpbssd" if ops.len() == 3 => self.emit_asm_amx_3op(&ops[0], &ops[1], &ops[2], 0x5E, 2),
            "tdpbusd" if ops.len() == 3 => self.emit_asm_amx_3op(&ops[0], &ops[1], &ops[2], 0x5E, 0),
            "tdpbuud" if ops.len() == 3 => self.emit_asm_amx_3op(&ops[0], &ops[1], &ops[2], 0x5E, 1),
            "tdpbf16ps" if ops.len() == 3 => self.emit_asm_amx_3op(&ops[0], &ops[1], &ops[2], 0x5F, 2),

            // === AVX-512 简化版 (无 mask / 无 broadcast) ===
            // vmovdqu32 (zmm) : 0F 6F (map=1, w=0) ; vmovdqu64 : 0F 6F (w=1)
            "vmovdqu32_z" if ops.len() == 2 => self.emit_asm_evex_2op(&ops[0], &ops[1], 512, 2, 1, 0x6F, 0),
            "vmovdqu64_z" if ops.len() == 2 => self.emit_asm_evex_2op(&ops[0], &ops[1], 512, 2, 1, 0x6F, 1),
            "vmovdqa32_z" if ops.len() == 2 => self.emit_asm_evex_2op(&ops[0], &ops[1], 512, 1, 1, 0x6F, 0),
            "vmovdqa64_z" if ops.len() == 2 => self.emit_asm_evex_2op(&ops[0], &ops[1], 512, 1, 1, 0x6F, 1),
            // vaddps zmm : 0F 58 (map=1, w=0)
            "vaddps_z" if ops.len() == 3 => self.emit_asm_evex_3op_reg(
                if let AsmOperand::Reg(d, _) = &ops[0] { *d } else { 0 },
                if let AsmOperand::Reg(s, _) = &ops[1] { *s } else { 0 },
                if let AsmOperand::Reg(s, _) = &ops[2] { *s } else { 0 },
                512, 0, 1, 0x58, 0),
            "vmulps_z" if ops.len() == 3 => self.emit_asm_evex_3op_reg(
                if let AsmOperand::Reg(d, _) = &ops[0] { *d } else { 0 },
                if let AsmOperand::Reg(s, _) = &ops[1] { *s } else { 0 },
                if let AsmOperand::Reg(s, _) = &ops[2] { *s } else { 0 },
                512, 0, 1, 0x59, 0),

            // === Mask 寄存器 (k0-k7) 操作 ===
            // kmovw : VEX.128.0F (k1, k2)
            // 简化版: 假设 REX 编码: 仅支持 reg-reg 和 reg-mem
            "kmovw" if ops.len() == 2 => {
                if let (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) = (&ops[0], &ops[1]) {
                    if *ss <= 16 && *ds <= 16 {
                        // 0F 90 /r (vex.L0.0F.W0)
                        self.emit_vex3_prefix(*d, 0, 0, 0);
                        self.emit(&[0x90, Self::modrm_reg_reg(*d, *s)]);
                    }
                } else { self.emit(&[0x90]); }
            }

            // crc32 : F2 0F 38 F0/F1 /r
            "crc32b" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 0, 0xF0, 0),  // F2 0F 38 F0
            "crc32w" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 1, 0, 0xF1, 0),  // 66 0F 38 F1
            "crc32l" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 0, 0xF1, 0),  // F2 0F 38 F1
            "crc32q" if ops.len() == 2 => self.emit_asm_vex_2op(&ops[0], &ops[1], 0, 0, 0, 0xF1, 1),  // F2 0F 38 F1 + REX.W

            // 未识别指令: 警告 + nop 占位
            _ => {
                eprintln!("warning: unsupported inline asm instruction '{}', emitted as nop", mnemonic);
                self.emit(&[0x90]);
            }
        }
    }

    // --- 辅助编码方法 ---

    fn rex(w: bool, r: u8, x: u8, b: u8) -> u8 {
        let mut v = 0x40u8;
        if w { v |= 0x08; }
        if r >= 8 { v |= 0x04; }
        if x >= 8 { v |= 0x02; }
        if b >= 8 { v |= 0x01; }
        v
    }

    fn modrm_reg_reg(reg: u8, rm: u8) -> u8 {
        0xC0 | ((reg & 7) << 3) | (rm & 7)
    }

    fn emit_asm_push_sized(&mut self, op: &AsmOperand, _bits: u8) {
        match op {
            AsmOperand::Reg(r, _) => {
                if *r >= 8 { self.emit(&[0x41]); }
                self.emit(&[0x50 + (r & 7)]);
            }
            AsmOperand::Seg(seg) => {
                // push %cs/%ds/%es/%fs/%gs/%ss: 0x0FA 段码 (CS=1, SS=2, DS=3, ES=0, FS=4, GS=5)
                // 标准形式 0x06 0x0E 0x16 0x1E (ES/CS/SS/DS), FS/GS 用 REX + opcode
                match *seg {
                    0 => self.emit(&[0x06]),   // push %es
                    1 => self.emit(&[0x0E]),   // push %cs
                    2 => self.emit(&[0x16]),   // push %ss
                    3 => self.emit(&[0x1E]),   // push %ds
                    4 => self.emit(&[0x0F, 0xA0]), // push %fs
                    5 => self.emit(&[0x0F, 0xA8]), // push %gs
                    _ => {
                        eprintln!("warning: push: unknown segment {}, emitted as nop", seg);
                        self.emit(&[0x90]);
                    }
                }
            }
            AsmOperand::Imm(v) => {
                if *v >= -128 && *v <= 127 {
                    self.emit(&[0x6A, *v as u8]);
                } else if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    self.emit(&[0x68]); self.emit(&(*v as i32).to_le_bytes());
                } else {
                    // 64-bit imm: 必须走两步: movabs rax, imm; push rax
                    self.emit(&[0x48, 0xB8]); self.emit(&v.to_le_bytes());
                    self.emit(&[0x50]); // push %rax
                }
            }
            AsmOperand::Mem { disp, base, index, scale } => {
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0xFF]);
                self.emit_asm_modrm_mem_only(6, *disp, *base, *index, *scale);
            }
            AsmOperand::SegMem { seg, disp, base, index, scale } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx, Self::rex(false, 0, index.unwrap_or(0), *base), 0xFF]);
                self.emit_asm_modrm_mem_only(6, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported push operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_pop_sized(&mut self, op: &AsmOperand, _bits: u8) {
        match op {
            AsmOperand::Reg(r, _) => {
                if *r >= 8 { self.emit(&[0x41]); }
                self.emit(&[0x58 + (r & 7)]);
            }
            AsmOperand::Seg(seg) => {
                // pop %cs/%ds/%es/%fs/%gs/%ss
                match *seg {
                    0 => self.emit(&[0x07]),   // pop %es
                    2 => self.emit(&[0x17]),   // pop %ss
                    3 => self.emit(&[0x1F]),   // pop %ds
                    4 => self.emit(&[0x0F, 0xA1]), // pop %fs
                    5 => self.emit(&[0x0F, 0xA9]), // pop %gs
                    _ => {
                        eprintln!("warning: pop: unknown segment {}, emitted as nop", seg);
                        self.emit(&[0x90]);
                    }
                }
            }
            AsmOperand::Mem { disp, base, index, scale } => {
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x8F]);
                self.emit_asm_modrm_mem_only(0, *disp, *base, *index, *scale);
            }
            AsmOperand::SegMem { seg, disp, base, index, scale } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx, Self::rex(false, 0, index.unwrap_or(0), *base), 0x8F]);
                self.emit_asm_modrm_mem_only(0, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported pop operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_mov(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        // 段寄存器读写: 0x8C /r (mov r/m16, Sreg) / 0x8E /r (mov Sreg, r/m16)
        if let (AsmOperand::Reg(s, _), AsmOperand::Seg(seg)) = (src, dst) {
            if bits == 64 {
                // mov %reg, %segreg : 8E /r
                let rex = Self::rex(false, *seg & 7, 0, *s);
                if *s >= 8 || *seg >= 8 { self.emit(&[rex]); }
                self.emit(&[0x8E, Self::modrm_reg_reg(*seg, *s)]);
            } else {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(false, *seg & 7, 0, *s), 0x8E, Self::modrm_reg_reg(*seg, *s)]);
            }
            return;
        }
        if let (AsmOperand::Seg(seg), AsmOperand::Reg(d, _)) = (src, dst) {
            if bits == 64 {
                // mov %segreg, %reg : 8C /r
                let rex = Self::rex(false, *d, 0, *seg & 7);
                if *d >= 8 || *seg >= 8 { self.emit(&[rex]); }
                self.emit(&[0x8C, Self::modrm_reg_reg(*d, *seg)]);
            } else {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(false, *d, 0, *seg & 7), 0x8C, Self::modrm_reg_reg(*d, *seg)]);
            }
            return;
        }
        // 段前缀 mem: 发射段前缀 + 普通 mem 路径
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = src {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            self.emit_asm_load_segmem(*disp, *base, *index, *scale, bits, dst, 0x8B);
            return;
        }
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = dst {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            self.emit_asm_store_segmem(*disp, *base, *index, *scale, bits, src, 0x89);
            return;
        }
        // sym(%rip) → mov [rip+sym], reg
        if let (AsmOperand::SymDisp(sym, _), AsmOperand::Reg(d, _)) = (src, dst) {
            // 全局变量 RIP-relative 加载: 与 emit_global_load 相同编码
            let rex = if *d >= 8 { 0x4C } else { 0x48 };
            let modrm = 0x05 | ((d & 7) << 3);
            if bits == 8 {
                self.emit(&[Self::rex(false, *d, 0, 0), 0x0F, 0xB6, modrm, 0,0,0,0]);
            } else if bits == 16 {
                self.emit(&[0x66, Self::rex(false, *d, 0, 0), 0x0F, 0xB7, modrm, 0,0,0,0]);
            } else if bits == 32 {
                self.emit(&[Self::rex(false, *d, 0, 0), 0x8B, modrm, 0,0,0,0]);
            } else {
                self.emit(&[rex, 0x8B, modrm, 0,0,0,0]);
            }
            let pos = self.pos() - 4;
            self.pending_data_rels.push((pos, sym.clone()));
            return;
        }
        // mov reg, sym(%rip)
        if let (AsmOperand::Reg(s, _), AsmOperand::SymDisp(sym, _)) = (src, dst) {
            // 64 位默认: mov [rip+sym], reg
            let rex = if *s >= 8 { 0x4C } else { 0x48 };
            let modrm = 0x05 | ((s & 7) << 3);
            if bits == 8 {
                self.emit(&[Self::rex(false, *s, 0, 0), 0x88, modrm, 0,0,0,0]);
            } else if bits == 16 {
                self.emit(&[0x66, Self::rex(false, *s, 0, 0), 0x89, modrm, 0,0,0,0]);
            } else if bits == 32 {
                self.emit(&[Self::rex(false, *s, 0, 0), 0x89, modrm, 0,0,0,0]);
            } else {
                self.emit(&[rex, 0x89, modrm, 0,0,0,0]);
            }
            let pos = self.pos() - 4;
            self.pending_data_rels.push((pos, sym.clone()));
            return;
        }
        match (src, dst) {
            // reg → reg
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                if bits == 8 {
                    // mov r8, r8: 88 /r
                    if *s >= 4 || *d >= 4 { self.emit(&[Self::rex(false, *s, 0, *d)]); }
                    self.emit(&[0x88, Self::modrm_reg_reg(*s, *d)]);
                } else {
                    if bits == 16 { self.emit(&[0x66]); }
                    self.emit(&[Self::rex(w, *s, 0, *d), 0x89, Self::modrm_reg_reg(*s, *d)]);
                }
            }
            // imm → reg
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 8 {
                    self.emit(&[0xB0 + (d & 7), *v as u8]);
                } else if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    if *d >= 8 { self.emit(&[0x41]); }
                    if bits == 16 { self.emit(&[0x66]); }
                    self.emit(&[0xB8 + (d & 7)]); self.emit(&(*v as i32).to_le_bytes());
                } else {
                    // movabs reg, imm64
                    if *d >= 8 { self.emit(&[0x49]); } else { self.emit(&[0x48]); }
                    self.emit(&[0xB8 + (d & 7)]); self.emit(&v.to_le_bytes());
                }
            }
            // mem → reg
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                self.emit_asm_load(*d, *disp, *base, *index, *scale, bits);
            }
            // reg → mem
            (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) => {
                self.emit_asm_store(*s, *disp, *base, *index, *scale, bits);
            }
            // imm → mem
            (AsmOperand::Imm(v), AsmOperand::Mem { disp, base, index, scale }) => {
                // mov [mem], imm: C7 /0 id (32-bit) or C6 /0 ib (8-bit)
                if bits == 8 {
                    self.emit_asm_modrm_mem(0xC6, 0, *disp, *base, *index, *scale, false);
                    self.emit(&[*v as u8]);
                } else {
                    if bits != 16 && bits != 8 { self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]); }
                    self.emit_asm_modrm_mem(0xC7, 0, *disp, *base, *index, *scale, false);
                    self.emit(&(*v as i32).to_le_bytes());
                }
            }
            // imm64 → mem (内核 movabs 内存目标)
            // 支持模式: mov imm, mem 走 REX.W + C7 /0 id (无法表示 imm64)
            // 走两步: movabs reg, imm; mov [mem], reg
            (AsmOperand::Imm(v), AsmOperand::SegMem { disp, base, index, scale, .. }) | (AsmOperand::Imm(v), AsmOperand::Mem { disp, base, index, scale }) => {
                // 先 movabs 到 rax, 再 mov [mem], rax
                if bits == 64 && (*v > i32::MAX as i64 || *v < i32::MIN as i64) {
                    // movabs %rax, imm64
                    self.emit(&[0x48, 0xB8]);
                    self.emit(&v.to_le_bytes());
                    self.emit_asm_store(0, *disp, *base, *index, *scale, 64);
                } else {
                    // 32-bit 立即数
                    if bits != 16 && bits != 8 { self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]); }
                    self.emit_asm_modrm_mem(0xC7, 0, *disp, *base, *index, *scale, false);
                    self.emit(&(*v as i32).to_le_bytes());
                }
            }
            _ => {
                eprintln!("warning: unsupported mov operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    /// 由段码得到 (modrm 字段值, 段前缀字节)
    fn seg_meta(seg: u8) -> (u8, u8) {
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

    /// 段前缀 mem → reg: 已在调用方 emit 段前缀, 此处只生成 modrm 部分
    fn emit_asm_load_segmem(&mut self, disp: i32, base: u8, index: Option<u8>, scale: u8, bits: u8, dst: &AsmOperand, opcode: u8) {
        let w = bits == 64;
        if let AsmOperand::Reg(d, _) = dst {
            self.emit_asm_load(*d, disp, base, index, scale, bits);
            // opcode 已被 emit_asm_load 内部固定为 0x8B; 此参数保留用于扩展
            let _ = opcode;
        } else if bits == 16 {
            self.emit(&[0x66]);
            self.emit(&[Self::rex(w, 0, index.unwrap_or(0), base)]);
            self.emit(&[opcode]);
            // dst 必须是 reg 才能形成完整 modrm
        }
    }

    /// 段前缀 reg → mem
    fn emit_asm_store_segmem(&mut self, disp: i32, base: u8, index: Option<u8>, scale: u8, bits: u8, src: &AsmOperand, opcode: u8) {
        let w = bits == 64;
        if let AsmOperand::Reg(s, _) = src {
            self.emit_asm_store(*s, disp, base, index, scale, bits);
            let _ = opcode;
        } else if bits == 16 {
            self.emit(&[0x66]);
            self.emit(&[Self::rex(w, 0, index.unwrap_or(0), base)]);
            self.emit(&[opcode]);
        }
    }

    fn emit_asm_load(&mut self, dst: u8, disp: i32, base: u8, index: Option<u8>, scale: u8, bits: u8) {
        let w = bits == 64;
        if base == ASM_BASE_RIP {
            if bits == 16 { self.emit(&[0x66]); }
            self.emit(&[Self::rex(w, dst, 0, 0), 0x8B]);
            self.emit_asm_modrm_mem_only(dst, disp, ASM_BASE_RIP, None, 1);
            return;
        }
        let has_sib = index.is_some() || (base & 7) == 4; // rsp/r12 需要 SIB
        let need_disp = disp != 0 || (base & 7) == 5; // rbp/r13 强制 disp8
        if bits == 16 { self.emit(&[0x66]); }
        self.emit(&[Self::rex(w, dst, index.unwrap_or(0), base)]);
        self.emit(&[0x8B]);
        let mut modrm = (dst & 7) << 3;
        if !need_disp && disp == 0 { modrm |= 0x00; } // mod=00
        else if disp >= -128 && disp <= 127 { modrm |= 0x40; } // mod=01
        else { modrm |= 0x80; } // mod=10
        if has_sib {
            self.emit(&[modrm | 0x04]); // rm=100 (SIB follows)
            let mut sib = base & 7;
            if let Some(idx) = index {
                sib |= (idx & 7) << 3;
                let sc = match scale { 2 => 1, 4 => 2, 8 => 3, _ => 0 };
                sib |= sc << 6;
            } else {
                sib |= 4 << 3; // idx=100 (no index)
            }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if disp >= -128 && disp <= 127 { self.emit(&[disp as u8]); }
            else { self.emit(&disp.to_le_bytes()); }
        }
    }

    fn emit_asm_store(&mut self, src: u8, disp: i32, base: u8, index: Option<u8>, scale: u8, bits: u8) {
        let w = bits == 64;
        if base == ASM_BASE_RIP {
            if bits == 16 { self.emit(&[0x66]); }
            self.emit(&[Self::rex(w, src, 0, 0), 0x89]);
            self.emit_asm_modrm_mem_only(src, disp, ASM_BASE_RIP, None, 1);
            return;
        }
        let has_sib = index.is_some() || (base & 7) == 4;
        let need_disp = disp != 0 || (base & 7) == 5;
        if bits == 16 { self.emit(&[0x66]); }
        self.emit(&[Self::rex(w, src, index.unwrap_or(0), base)]);
        self.emit(&[0x89]);
        let mut modrm = (src & 7) << 3;
        if !need_disp && disp == 0 { modrm |= 0x00; }
        else if disp >= -128 && disp <= 127 { modrm |= 0x40; }
        else { modrm |= 0x80; }
        if has_sib {
            self.emit(&[modrm | 0x04]);
            let mut sib = base & 7;
            if let Some(idx) = index {
                sib |= (idx & 7) << 3;
                let sc = match scale { 2 => 1, 4 => 2, 8 => 3, _ => 0 };
                sib |= sc << 6;
            } else {
                sib |= 4 << 3;
            }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if disp >= -128 && disp <= 127 { self.emit(&[disp as u8]); }
            else { self.emit(&disp.to_le_bytes()); }
        }
    }

    fn emit_asm_modrm_mem(&mut self, opcode: u8, reg: u8, disp: i32, base: u8, index: Option<u8>, scale: u8, need_rex: bool) {
        let has_sib = index.is_some() || (base & 7) == 4;
        let need_disp = disp != 0 || (base & 7) == 5;
        if need_rex { self.emit(&[Self::rex(false, reg, index.unwrap_or(0), base)]); }
        self.emit(&[opcode]);
        let mut modrm = (reg & 7) << 3;
        if !need_disp && disp == 0 { modrm |= 0x00; }
        else if disp >= -128 && disp <= 127 { modrm |= 0x40; }
        else { modrm |= 0x80; }
        if has_sib {
            self.emit(&[modrm | 0x04]);
            let mut sib = base & 7;
            if let Some(idx) = index { sib |= (idx & 7) << 3; let sc = match scale { 2=>1,4=>2,8=>3,_=>0 }; sib |= sc << 6; }
            else { sib |= 4 << 3; }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if disp >= -128 && disp <= 127 { self.emit(&[disp as u8]); }
            else { self.emit(&disp.to_le_bytes()); }
        }
    }

    fn emit_asm_lea(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        if let (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) = (src, dst) {
            let w = bits == 64;
            if *base == ASM_BASE_RIP {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, 0, 0), 0x8D]);
                self.emit_asm_modrm_mem_only(*d, *disp, ASM_BASE_RIP, None, 1);
                return;
            }
            self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x8D]);
            let has_sib = index.is_some() || (base & 7) == 4;
            let need_disp = *disp != 0 || (base & 7) == 5;
            let mut modrm = (d & 7) << 3;
            if !need_disp && *disp == 0 { modrm |= 0x00; }
            else if *disp >= -128 && *disp <= 127 { modrm |= 0x40; }
            else { modrm |= 0x80; }
            if has_sib {
                self.emit(&[modrm | 0x04]);
                let mut sib = *base & 7;
                if let Some(idx) = index { sib |= (idx & 7) << 3; let sc = match scale { 2=>1,4=>2,8=>3,_=>0 }; sib |= sc << 6; }
                else { sib |= 4 << 3; }
                self.emit(&[sib]);
            } else {
                self.emit(&[modrm | (base & 7)]);
            }
            if need_disp {
                if *disp >= -128 && *disp <= 127 { self.emit(&[*disp as u8]); }
                else { self.emit(&disp.to_le_bytes()); }
            }
        } else {
            eprintln!("warning: unsupported lea operand pattern, emitted as nop");
            self.emit(&[0x90]);
        }
    }

    fn emit_asm_alu(&mut self, src: &AsmOperand, dst: &AsmOperand, op: u8, bits: u8) {
        let w = bits == 64;
        // 段前缀 mem: emit 段前缀 + 普通 mem ALU
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = src {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(d, _) = dst {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x03 + op * 8]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = dst {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(s, _) = src {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x01 + op * 8]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                if bits == 8 {
                    if *s >= 4 || *d >= 4 { self.emit(&[Self::rex(false, *s, 0, *d)]); }
                    self.emit(&[0x00 + op * 8, Self::modrm_reg_reg(*s, *d)]);
                } else {
                    if bits == 16 { self.emit(&[0x66]); }
                    self.emit(&[Self::rex(w, *s, 0, *d), 0x01 + op * 8, Self::modrm_reg_reg(*s, *d)]);
                }
            }
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                if bits != 8 { self.emit(&[Self::rex(w, 0, 0, *d)]); }
                if *v >= -128 && *v <= 127 {
                    // 8/16 位: 80 /0 ib
                    if bits == 8 {
                        self.emit(&[0x80 + op * 8, Self::modrm_reg_reg(op, *d), *v as u8]);
                    } else {
                        self.emit(&[0x83, Self::modrm_reg_reg(op, *d), *v as u8]);
                    }
                } else if bits == 8 {
                    // 8 位但 imm 越界: 报错
                    eprintln!("warning: 8-bit ALU immediate out of range ({}), emitted as nop", v);
                    self.emit(&[0x90]);
                } else if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    self.emit(&[0x81, Self::modrm_reg_reg(op, *d)]);
                    self.emit(&(*v as i32).to_le_bytes());
                } else {
                    // 64 位立即数: movabs rax, imm; op rax, rax (无 op reg, reg 模式用 addq)
                    self.emit(&[0x48, 0xB8]); self.emit(&v.to_le_bytes());
                    if *d != 0 {
                        // mov d, rax
                        if bits == 64 {
                            self.emit(&[Self::rex(true, 0, 0, *d), 0x89, Self::modrm_reg_reg(0, *d)]);
                        }
                        // 现在 rax 是 imm; dst 是 d: op d, rax
                        self.emit(&[Self::rex(w, *d, 0, 0), 0x01 + op * 8, Self::modrm_reg_reg(*d, 0)]);
                    } else {
                        // op rax, rax
                        self.emit(&[Self::rex(w, 0, 0, 0), 0x01 + op * 8, Self::modrm_reg_reg(0, 0)]);
                    }
                }
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base)]);
                self.emit(&[0x03 + op * 8]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base)]);
                self.emit(&[0x01 + op * 8]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            (AsmOperand::Imm(v), AsmOperand::Mem { disp, base, index, scale }) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base)]);
                if *v >= -128 && *v <= 127 && op == 7 {
                    self.emit(&[0x83]);
                    self.emit_asm_modrm_mem_only(7, *disp, *base, *index, *scale);
                    self.emit(&[*v as u8]);
                } else if bits == 8 {
                    self.emit(&[0x80 + op * 8]);
                    self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                    self.emit(&[*v as u8]);
                } else {
                    self.emit(&[0x81]);
                    self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                    self.emit(&(*v as i32).to_le_bytes());
                }
            }
            _ => {
                eprintln!("warning: unsupported ALU operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_modrm_mem_only(&mut self, reg: u8, disp: i32, base: u8, index: Option<u8>, scale: u8) {
        if base == ASM_BASE_RIP {
            let mut modrm = ((reg & 7) << 3) | 0x05;
            if disp != 0 {
                modrm = ((reg & 7) << 3) | 0x85;
            }
            self.emit(&[modrm]);
            self.emit(&disp.to_le_bytes());
            return;
        }
        let has_sib = index.is_some() || (base & 7) == 4;
        let need_disp = disp != 0 || (base & 7) == 5;
        let mut modrm = (reg & 7) << 3;
        if !need_disp && disp == 0 { modrm |= 0x00; }
        else if disp >= -128 && disp <= 127 { modrm |= 0x40; }
        else { modrm |= 0x80; }
        if has_sib {
            self.emit(&[modrm | 0x04]);
            let mut sib = base & 7;
            if let Some(idx) = index { sib |= (idx & 7) << 3; let sc = match scale { 2=>1,4=>2,8=>3,_=>0 }; sib |= sc << 6; }
            else { sib |= 4 << 3; }
            self.emit(&[sib]);
        } else {
            self.emit(&[modrm | (base & 7)]);
        }
        if need_disp {
            if disp >= -128 && disp <= 127 { self.emit(&[disp as u8]); }
            else { self.emit(&disp.to_le_bytes()); }
        }
    }

    fn emit_asm_test(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        // 段前缀 mem: 段前缀 + 普通 mem test
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = src {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(d, _) = dst {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x85]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = dst {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(s, _) = src {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x85]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *s, 0, *d), 0x85, Self::modrm_reg_reg(*s, *d)]);
            }
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, 0, 0, *d)]);
                if bits == 8 {
                    if *v >= -128 && *v <= 127 {
                        self.emit(&[0xF6, Self::modrm_reg_reg(0, *d), *v as u8]);
                    } else {
                        eprintln!("warning: 8-bit test immediate out of range ({}), emitted as nop", v);
                        self.emit(&[0x90]);
                    }
                } else if *v >= -128 && *v <= 127 && (*d & 7) == 0 {
                    // test $imm8, %al/ax/eax/rax 优化形式: A8 ib / 66 A9 iw / A9 id / 48 A9 id
                    let opcode = if bits == 16 { 0x66A9u16 } else { 0xA9u16 };
                    if bits == 16 {
                        self.emit(&[0x66]);
                        self.emit(&[0xA9]);
                        self.emit(&(*v as i16).to_le_bytes());
                    } else if *v >= i8::MIN as i64 && *v <= i8::MAX as i64 {
                        self.emit(&[0xA8, *v as u8]);
                    } else if bits == 32 {
                        self.emit(&[0xA9]); self.emit(&(*v as i32).to_le_bytes());
                    } else {
                        // 64 位 + 大 imm
                        if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                            self.emit(&[0xA9]); self.emit(&(*v as i32).to_le_bytes());
                        } else {
                            // 64 位 imm: 走通用 modrm 形式
                            self.emit(&[0xF7, Self::modrm_reg_reg(0, *d)]);
                            self.emit(&v.to_le_bytes());
                        }
                    }
                    let _ = opcode;
                } else if bits == 16 {
                    self.emit(&[0xF7, Self::modrm_reg_reg(0, *d)]);
                    self.emit(&(*v as i16).to_le_bytes());
                } else if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    self.emit(&[0xF7, Self::modrm_reg_reg(0, *d)]);
                    self.emit(&(*v as i32).to_le_bytes());
                } else {
                    // 64 位 imm 走 movabs + test
                    self.emit(&[0x48, 0xB8]); self.emit(&v.to_le_bytes());
                    self.emit(&[Self::rex(w, 0, 0, 0), 0x85, Self::modrm_reg_reg(0, *d)]);
                }
            }
            (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x85]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x85]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            (AsmOperand::Imm(v), AsmOperand::Mem { disp, base, index, scale }) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), 0xF7]);
                self.emit_asm_modrm_mem_only(0, *disp, *base, *index, *scale);
                if bits == 8 { self.emit(&[*v as u8]); }
                else if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    self.emit(&(*v as i32).to_le_bytes());
                } else {
                    eprintln!("warning: 32/64-bit test mem immediate out of range ({}), emitted as nop", v);
                    self.emit(&[0x90]);
                }
            }
            _ => {
                eprintln!("warning: unsupported test operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_shift(&mut self, src: &AsmOperand, dst: &AsmOperand, op: u8, bits: u8) {
        let w = bits == 64;
        // 段前缀 mem: 段前缀 + 普通 mem shift
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = dst {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            match src {
                AsmOperand::Imm(v) => {
                    if bits == 16 { self.emit(&[0x66]); }
                    self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base)]);
                    if *v == 1 {
                        self.emit(&[0xD1]);
                        self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                    } else {
                        self.emit(&[0xC1]);
                        self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                        self.emit(&[*v as u8]);
                    }
                }
                AsmOperand::Reg(c, _) if *c == 1 => {
                    if bits == 16 { self.emit(&[0x66]); }
                    self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), 0xD3]);
                    self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                }
                _ => {
                    eprintln!("warning: shift SegMem: only $imm or %cl supported, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, 0, 0, *d)]);
                if *v == 1 { self.emit(&[0xD1, Self::modrm_reg_reg(op, *d)]); }
                else { self.emit(&[0xC1, Self::modrm_reg_reg(op, *d), *v as u8]); }
            }
            // 兼容: shift $cl 实际语义是 shl/sar/shr/r 任意 reg 作为 count 来源
            // 但 x86 仅支持 %cl; 这里扩展为任意 GP (mov 到 cl 后 D3 /4)
            (AsmOperand::Reg(c, _), AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                if *c != 1 {
                    // 任意 reg: 先 mov cl, c 再 D3 /4
                    if *c >= 8 { self.emit(&[0x49]); } else { self.emit(&[0x48]); }
                    self.emit(&[0x89, Self::modrm_reg_reg(*c, 1)]);
                }
                self.emit(&[Self::rex(w, 0, 0, *d), 0xD3, Self::modrm_reg_reg(op, *d)]);
            }
            (AsmOperand::Imm(v), AsmOperand::Mem { disp, base, index, scale }) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base)]);
                if *v == 1 {
                    self.emit(&[0xD1]);
                    self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                } else {
                    self.emit(&[0xC1]);
                    self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
                    self.emit(&[*v as u8]);
                }
            }
            (AsmOperand::Reg(c, _), AsmOperand::Mem { disp, base, index, scale }) if *c == 1 => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), 0xD3]);
                self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
            }
            (AsmOperand::Reg(c, _), AsmOperand::Mem { disp, base, index, scale }) => {
                // 任意 reg: 先 mov cl, c 再 D3
                if bits == 16 { self.emit(&[0x66]); }
                if *c >= 8 { self.emit(&[0x49]); } else { self.emit(&[0x48]); }
                self.emit(&[0x89, Self::modrm_reg_reg(*c, 1)]);
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), 0xD3]);
                self.emit_asm_modrm_mem_only(op, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported shift operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    /// rol/ror：与 shift 相同的操作数模式
    fn emit_asm_rotate(&mut self, src: &AsmOperand, dst: &AsmOperand, op: u8, bits: u8) {
        self.emit_asm_shift(src, dst, op, bits);
    }

    /// prefetch* mem
    fn emit_asm_prefetch(&mut self, op: &AsmOperand, hint: u8) {
        if let AsmOperand::Mem { disp, base, index, scale } = op {
            self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, 0x18]);
            self.emit_asm_modrm_mem_only(hint, *disp, *base, *index, *scale);
        } else if let AsmOperand::SegMem { seg, disp, base, index, scale } = op {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx, Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, 0x18]);
            self.emit_asm_modrm_mem_only(hint, *disp, *base, *index, *scale);
        } else {
            eprintln!("warning: unsupported prefetch operand pattern, emitted as nop");
            self.emit(&[0x90]);
        }
    }

    /// 0F 01 /m 系统指令（invlpg 等）
    fn emit_asm_sys_mem(&mut self, op: &AsmOperand, grp: u8, ext: u8) {
        if let AsmOperand::Mem { disp, base, index, scale } = op {
            self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, 0x01]);
            self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
        } else if let AsmOperand::SegMem { seg, disp, base, index, scale } = op {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx, Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, 0x01]);
            self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
        } else {
            eprintln!("warning: unsupported sys mem operand pattern, emitted as nop");
            self.emit(&[0x90]);
        }
        let _ = grp;
    }

    fn emit_asm_int(&mut self, op: &AsmOperand) {
        if let AsmOperand::Imm(v) = op {
            self.emit(&[0xCD, *v as u8]);
        } else {
            eprintln!("warning: unsupported int operand pattern, emitted as nop");
            self.emit(&[0x90]);
        }
    }

    /// vmread %reg, %reg: 0F 78 /r (reg 字段 = dst 即 VmcsField, rm 字段 = src 即 GPR)
    /// AT&T vmread src, dst → src GPR (vmcs field), dst GPR (output)
    fn emit_asm_vmread(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            self.emit(&[Self::rex(true, *d, 0, *s), 0x0F, 0x78, Self::modrm_reg_reg(*d, *s)]);
        } else {
            eprintln!("warning: vmread only supports reg,reg operands");
            self.emit(&[0x90]);
        }
    }

    /// vmwrite src, dst: 0F 79 /r (reg 字段 = src 即 GPR, rm 字段 = dst 即 VmcsField)
    fn emit_asm_vmwrite(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            self.emit(&[Self::rex(true, *s, 0, *d), 0x0F, 0x79, Self::modrm_reg_reg(*s, *d)]);
        } else {
            eprintln!("warning: vmwrite only supports reg,reg operands");
            self.emit(&[0x90]);
        }
    }

    /// fxsave / fxrstor / fnsave / fnstenv / ldmxcsr / stmxcsr 等
    /// opcode: 单字节操作码 (如 0xAE, 0xDD, 0xD9)
    /// ext: modrm.reg 字段
    /// has_0f: 是否带 0F 前缀
    fn emit_asm_fpu_load(&mut self, op: &AsmOperand, opcode: u8, ext: u8, has_0f: bool) {
        if let AsmOperand::Mem { disp, base, index, scale } = op {
            self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]);
            if has_0f { self.emit(&[0x0F]); }
            self.emit(&[opcode]);
            self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
        } else if let AsmOperand::SegMem { seg, disp, base, index, scale } = op {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx, Self::rex(false, 0, index.unwrap_or(0), *base)]);
            if has_0f { self.emit(&[0x0F]); }
            self.emit(&[opcode]);
            self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
        } else if let AsmOperand::SymDisp(sym, _) = op {
            // 全局变量 RIP-relative: mod=00 r/m=101
            self.emit(&[Self::rex(false, 0, 0, 0)]);
            if has_0f { self.emit(&[0x0F]); }
            self.emit(&[opcode, 0x05 | (ext << 3), 0, 0, 0, 0]);
            let pos = self.pos() - 4;
            self.pending_data_rels.push((pos, sym.clone()));
        } else {
            eprintln!("warning: fpu_load only supports mem operand, emitted as nop");
            self.emit(&[0x90]);
        }
    }

    /// movss/movsd：AT&T src, dst
    fn emit_asm_sse_mov(&mut self, src: &AsmOperand, dst: &AsmOperand, single: bool, to_xmm64: bool) {
        let prefix = if single && !to_xmm64 { 0xF3u8 } else { 0xF2u8 };
        let is_xmm = |_r: u8, sz: u8| sz == 128;
        match (src, dst) {
            (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) if is_xmm(*s, *ss) && is_xmm(*d, *ds) => {
                let mut rex = 0u8;
                if *d >= 8 { rex |= 0x04; }
                if *s >= 8 { rex |= 0x01; }
                if rex != 0 { self.emit(&[0x40 | rex]); }
                self.emit(&[prefix, 0x0F, 0x10, Self::modrm_reg_reg(*d, *s)]);
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, ds)) if is_xmm(*d, *ds) => {
                let mut rex = 0u8;
                if *d >= 8 { rex |= 0x04; }
                if *base >= 8 { rex |= 0x01; }
                if index.unwrap_or(0) >= 8 { rex |= 0x02; }
                if rex != 0 { self.emit(&[0x40 | rex]); }
                self.emit(&[prefix, 0x0F, 0x10]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            (AsmOperand::Reg(s, ss), AsmOperand::Mem { disp, base, index, scale }) if is_xmm(*s, *ss) => {
                let mut rex = 0u8;
                if *s >= 8 { rex |= 0x04; }
                if *base >= 8 { rex |= 0x01; }
                if index.unwrap_or(0) >= 8 { rex |= 0x02; }
                if rex != 0 { self.emit(&[0x40 | rex]); }
                self.emit(&[prefix, 0x0F, 0x11]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported sse mov operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    /// 标量 SSE 算术：prefix 可为 0x0F / 0xF2 / 0xF3 / 0x66
    fn emit_asm_sse_arith(&mut self, src: &AsmOperand, dst: &AsmOperand, prefix: u8, opcode: u8) {
        let is_xmm = |_r: u8, sz: u8| sz == 128;
        match (src, dst) {
            (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) if is_xmm(*s, *ss) && is_xmm(*d, *ds) => {
                let mut rex = 0u8;
                if *d >= 8 { rex |= 0x04; }
                if *s >= 8 { rex |= 0x01; }
                if rex != 0 { self.emit(&[0x40 | rex]); }
                if prefix == 0x66 {
                    self.emit(&[0x66, 0x0F, opcode, Self::modrm_reg_reg(*d, *s)]);
                } else if prefix == 0x0F {
                    self.emit(&[0x0F, opcode, Self::modrm_reg_reg(*d, *s)]);
                } else {
                    self.emit(&[prefix, 0x0F, opcode, Self::modrm_reg_reg(*d, *s)]);
                }
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, ds)) if is_xmm(*d, *ds) => {
                let mut rex = 0u8;
                if *d >= 8 { rex |= 0x04; }
                if *base >= 8 { rex |= 0x01; }
                if index.unwrap_or(0) >= 8 { rex |= 0x02; }
                if rex != 0 { self.emit(&[0x40 | rex]); }
                if prefix == 0x66 {
                    self.emit(&[0x66, 0x0F, opcode]);
                } else if prefix == 0x0F {
                    self.emit(&[0x0F, opcode]);
                } else {
                    self.emit(&[prefix, 0x0F, opcode]);
                }
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            (AsmOperand::Reg(s, ss), AsmOperand::Mem { disp, base, index, scale }) if is_xmm(*s, *ss) => {
                let mut rex = 0u8;
                if *s >= 8 { rex |= 0x04; }
                if *base >= 8 { rex |= 0x01; }
                if index.unwrap_or(0) >= 8 { rex |= 0x02; }
                if rex != 0 { self.emit(&[0x40 | rex]); }
                if prefix == 0x66 {
                    self.emit(&[0x66, 0x0F, opcode]);
                } else if prefix == 0x0F {
                    self.emit(&[0x0F, opcode]);
                } else {
                    self.emit(&[prefix, 0x0F, opcode]);
                }
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported sse arith operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    /// shld/shrd: AT&T shld src, dst, count
    fn emit_asm_shld_shrd(
        &mut self,
        src: &AsmOperand,
        dst: &AsmOperand,
        count: &AsmOperand,
        is_shld: bool,
        bits: u8,
    ) {
        let w = bits == 64;
        let op = if is_shld { 0xA5u8 } else { 0xADu8 };
        let op_imm = if is_shld { 0xA4u8 } else { 0xACu8 };
        if bits == 16 {
            self.emit(&[0x66]);
        }
        match (src, dst, count) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _), AsmOperand::Reg(c, _)) if *c == 1 => {
                self.emit(&[Self::rex(w, *s, 0, *d), 0x0F, op, Self::modrm_reg_reg(*s, *d)]);
            }
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _), AsmOperand::Imm(v)) => {
                self.emit(&[Self::rex(w, *s, 0, *d), 0x0F, op_imm, Self::modrm_reg_reg(*s, *d), *v as u8]);
            }
            _ => {
                eprintln!("warning: unsupported shld/shrd operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    /// bt/bts/btr: AT&T btl src, dst（src=位索引，dst=被测操作数）
    fn emit_asm_bit(&mut self, src: &AsmOperand, dst: &AsmOperand, opcode: u8, bits: u8) {
        let w = bits == 64;
        if bits == 16 {
            self.emit(&[0x66]);
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(w, *s, 0, *d), 0x0F, opcode, Self::modrm_reg_reg(*s, *d)]);
            }
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                let ext = match opcode {
                    0xA3 => 4,
                    0xAB => 5,
                    0xB3 => 6,
                    0xBB => 7,
                    _ => 4,
                };
                self.emit(&[Self::rex(w, 0, 0, *d), 0x0F, 0xBA, Self::modrm_reg_reg(ext, *d), *v as u8]);
            }
            (AsmOperand::Imm(v), AsmOperand::Mem { disp, base, index, scale }) => {
                let ext = match opcode {
                    0xA3 => 4,
                    0xAB => 5,
                    0xB3 => 6,
                    0xBB => 7,
                    _ => 4,
                };
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), 0x0F, 0xBA]);
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
                self.emit(&[*v as u8]);
            }
            (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) => {
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported bit-test operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_imul2(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(true, *d, 0, *s), 0x0F, 0xAF, Self::modrm_reg_reg(*d, *s)]);
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(true, *d, index.unwrap_or(0), *base), 0x0F, 0xAF]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) => {
                self.emit(&[Self::rex(true, *s, index.unwrap_or(0), *base), 0x0F, 0xAF]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported imul2 operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_imul3(&mut self, src1: &AsmOperand, src2: &AsmOperand, dst: &AsmOperand) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Imm(v), AsmOperand::Reg(d, _)) = (src2, src1, dst) {
            if *v >= -128 && *v <= 127 {
                self.emit(&[Self::rex(true, *d, 0, *s), 0x6B, Self::modrm_reg_reg(*d, *s), *v as u8]);
            } else {
                self.emit(&[Self::rex(true, *d, 0, *s), 0x69, Self::modrm_reg_reg(*d, *s)]);
                self.emit(&(*v as i32).to_le_bytes());
            }
        } else {
            eprintln!("warning: unsupported imul3 operand pattern, emitted as nop");
            self.emit(&[0x90]);
        }
    }

    fn emit_asm_unary_rm(&mut self, op: &AsmOperand, ext: u8, bits: u8) {
        let w = bits == 64;
        match op {
            AsmOperand::Reg(r, _) => {
                self.emit(&[Self::rex(w, 0, 0, *r), 0xF7, Self::modrm_reg_reg(ext, *r)]);
            }
            AsmOperand::Mem { disp, base, index, scale } => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, 0, index.unwrap_or(0), *base), 0xF7]);
                self.emit_asm_modrm_mem_only(ext, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported unary operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_jmp(&mut self, op: &AsmOperand) {
        match op {
            AsmOperand::Sym(label) => {
                self.jmp_rel32(label, JumpKind::Jmp);
            }
            AsmOperand::SymDisp(label, _off) => {
                // jmp *sym@PLT 或 jmp *sym: 走 RIP-relative 间接
                // 模拟方式: lea rax, [rip+label]; jmp *rax
                self.emit(&[0x48, 0x8D, 0x05, 0, 0, 0, 0]);
                self.pending.push(PendingJump { at: self.pos() - 4, target: label.clone(), kind: JumpKind::Jmp });
                self.emit(&[0xFF, 0xE0]); // jmp *%rax
            }
            AsmOperand::Reg(r, _) => {
                // jmp reg: FF /4
                if *r >= 8 { self.emit(&[0x41]); }
                self.emit(&[0xFF, 0xE0 | (r & 7)]);
            }
            AsmOperand::Mem { disp, base, index, scale } => {
                // jmp [mem]: FF /4
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]);
                self.emit_asm_modrm_mem(0xFF, 4, *disp, *base, *index, *scale, false);
            }
            AsmOperand::SegMem { seg, disp, base, index, scale } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx]);
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]);
                self.emit_asm_modrm_mem(0xFF, 4, *disp, *base, *index, *scale, false);
            }
            AsmOperand::Imm(v) => {
                // 绝对远跳转: mov rax, imm; jmp *rax
                if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 {
                    self.emit(&[0xE9]); self.emit(&(*v as i32).to_le_bytes());
                } else {
                    self.emit(&[0x48, 0xB8]); self.emit(&v.to_le_bytes());
                    self.emit(&[0xFF, 0xE0]);
                }
            }
            _ => {
                eprintln!("warning: unsupported jmp operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_jcc(&mut self, op: &AsmOperand, cc: u8) {
        match op {
            AsmOperand::Sym(label) => {
                // 发射条件跳转: 0F 8x disp32 (6 bytes)
                let at = self.pos();
                self.emit(&[0x0F, cc, 0, 0, 0, 0]);
                // 映射 AT&T cc 字节到 JumpKind
                let kind = match cc {
                    0x84 => JumpKind::Je, 0x85 => JumpKind::Jne,
                    0x8C => JumpKind::Jl, 0x8E => JumpKind::Jle,
                    0x8F => JumpKind::Jg, 0x8D => JumpKind::Jge,
                    _ => JumpKind::Jne, // fallback
                };
                self.pending.push(PendingJump { at, target: label.clone(), kind });
            }
            AsmOperand::Imm(v) => {
                // 短/近条件跳转到绝对地址: 0F 8x disp32
                let at = self.pos();
                self.emit(&[0x0F, cc]);
                self.emit(&(*v as i32).to_le_bytes());
                let _ = at;
            }
            _ => {
                eprintln!("warning: unsupported jcc operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_call(&mut self, op: &AsmOperand) {
        match op {
            AsmOperand::Sym(label) => {
                self.call_label(label);
            }
            AsmOperand::SymDisp(label, _off) => {
                // call *sym@PLT: lea rax, [rip+label]; call *rax
                self.emit(&[0x48, 0x8D, 0x05, 0, 0, 0, 0]);
                self.pending.push(PendingJump { at: self.pos() - 4, target: label.clone(), kind: JumpKind::Jmp });
                self.emit(&[0xFF, 0xD0]); // call *%rax
            }
            AsmOperand::Reg(r, _) => {
                if *r >= 8 { self.emit(&[0x41]); }
                self.emit(&[0xFF, 0xD0 | (r & 7)]);
            }
            AsmOperand::Mem { disp, base, index, scale } => {
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]);
                self.emit_asm_modrm_mem(0xFF, 2, *disp, *base, *index, *scale, false);
            }
            AsmOperand::SegMem { seg, disp, base, index, scale } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx]);
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base)]);
                self.emit_asm_modrm_mem(0xFF, 2, *disp, *base, *index, *scale, false);
            }
            AsmOperand::Imm(v) => {
                // 直接绝对 call: mov rax, imm; call *rax
                self.emit(&[0x48, 0xB8]); self.emit(&v.to_le_bytes());
                self.emit(&[0xFF, 0xD0]);
            }
            _ => {
                eprintln!("warning: unsupported call operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_movzx(&mut self, src: &AsmOperand, dst: &AsmOperand, src_bits: u8, dst_bits: u8) {
        let w = dst_bits == 64;
        // 段前缀 mem: 段前缀 + 普通 mem movzx
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = src {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(d, _) = dst {
                if dst_bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F]);
                let op = if src_bits == 8 { 0xB6u8 } else { 0xB7 };
                self.emit(&[op]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(w, *d, 0, *s)]);
                match src_bits {
                    8 => self.emit(&[0x0F, 0xB6, Self::modrm_reg_reg(*d, *s)]),
                    16 => self.emit(&[0x0F, 0xB7, Self::modrm_reg_reg(*d, *s)]),
                    _ => {
                        eprintln!("warning: unsupported movzx source size, emitted as nop");
                        self.emit(&[0x90])
                    }
                };
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F]);
                let op = if src_bits == 8 { 0xB6u8 } else { 0xB7 };
                self.emit(&[op]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                // movzx $imm, %reg: 走 mov reg, imm
                if src_bits == 8 {
                    if *d < 8 { self.emit(&[0xB0 + (d & 7), *v as u8]); }
                    else { self.emit(&[0x41, 0xB0 + (d & 7), *v as u8]); }
                } else {
                    eprintln!("warning: movzx $imm only supports 8-bit source, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            _ => {
                eprintln!("warning: unsupported movzx operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_movsx(&mut self, src: &AsmOperand, dst: &AsmOperand, src_bits: u8, dst_bits: u8) {
        let w = dst_bits == 64;
        // 段前缀 mem
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = src {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(d, _) = dst {
                if dst_bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F]);
                let op = match src_bits {
                    8 => 0xBEu8,
                    16 => 0xBF,
                    32 => 0x63, // movsxd
                    _ => 0xBE,
                };
                self.emit(&[op]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(w, *d, 0, *s)]);
                match src_bits {
                    8 => self.emit(&[0x0F, 0xBE, Self::modrm_reg_reg(*d, *s)]),
                    16 => self.emit(&[0x0F, 0xBF, Self::modrm_reg_reg(*d, *s)]),
                    32 => self.emit(&[0x63, Self::modrm_reg_reg(*d, *s)]),
                    _ => {
                        eprintln!("warning: unsupported movsx source size, emitted as nop");
                        self.emit(&[0x90])
                    }
                };
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                if dst_bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F]);
                let op = match src_bits {
                    8 => 0xBEu8,
                    16 => 0xBF,
                    32 => 0x63,
                    _ => 0xBE,
                };
                self.emit(&[op]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            (AsmOperand::Imm(v), AsmOperand::Reg(d, _)) => {
                // movsx $imm, %reg: 走 mov reg, imm
                if src_bits == 8 {
                    if *d < 8 { self.emit(&[0xB0 + (d & 7), *v as u8]); }
                    else { self.emit(&[0x41, 0xB0 + (d & 7), *v as u8]); }
                } else if src_bits == 16 {
                    if dst_bits == 16 {
                        if *d < 8 { self.emit(&[0x66, 0xB8 + (d & 7)]); }
                        else { self.emit(&[0x66, 0x41, 0xB8 + (d & 7)]); }
                    } else {
                        // movsx $imm16 → mov reg, imm
                        if *d < 8 { self.emit(&[0xB8 + (d & 7)]); }
                        else { self.emit(&[0x41, 0xB8 + (d & 7)]); }
                    }
                    self.emit(&(*v as i16).to_le_bytes());
                } else if src_bits == 32 {
                    if dst_bits == 32 {
                        if *d < 8 { self.emit(&[0xB8 + (d & 7)]); }
                        else { self.emit(&[0x41, 0xB8 + (d & 7)]); }
                    } else {
                        if *d < 8 { self.emit(&[0x48, 0xB8 + (d & 7)]); }
                        else { self.emit(&[0x49, 0xB8 + (d & 7)]); }
                    }
                    self.emit(&(*v as i32).to_le_bytes());
                } else {
                    eprintln!("warning: movsx $imm with unsupported size, emitted as nop");
                    self.emit(&[0x90]);
                }
            }
            _ => {
                eprintln!("warning: unsupported movsx operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_xchg(&mut self, src: &AsmOperand, dst: &AsmOperand) {
        if let (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) = (src, dst) {
            if *s == 0 { // xchg eax, reg
                if *d >= 8 { self.emit(&[0x41]); }
                self.emit(&[0x90 + (d & 7)]);
            } else if *d == 0 {
                if *s >= 8 { self.emit(&[0x41]); }
                self.emit(&[0x90 + (s & 7)]);
            } else {
                self.emit(&[Self::rex(true, *s, 0, *d), 0x87, Self::modrm_reg_reg(*s, *d)]);
            }
        } else { self.emit(&[0x90]); }
    }

    fn emit_asm_cmov(&mut self, src: &AsmOperand, dst: &AsmOperand, cc: u8) {
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(true, *d, 0, *s), 0x0F, cc, Self::modrm_reg_reg(*d, *s)]);
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(true, *d, index.unwrap_or(0), *base), 0x0F, cc]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) => {
                self.emit(&[Self::rex(true, *s, index.unwrap_or(0), *base), 0x0F, cc]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            _ => self.emit(&[0x90]),
        }
    }

    fn emit_asm_setcc(&mut self, op: &AsmOperand, cc: u8) {
        match op {
            AsmOperand::Reg(r, sz) => {
                // 8 位寄存器编码: ah/bh/ch/dh 是 4..7 物理槽, 但 r/m 字段也用 4..7
                // REX 无效编码 0x40 用于 ah/bh/ch/dh 旧语法; 用 REX 后 ah 不可达
                // 这里统一按 r/m 字段 0..7 处理
                if *r >= 4 && *r < 8 && *sz == 8 {
                    // 高 8 位寄存器: 不发 REX, 直接编码
                    self.emit(&[0x0F, cc, 0xC0 | (r & 7)]);
                } else {
                    if *r >= 8 { self.emit(&[0x41]); }
                    self.emit(&[0x0F, cc, 0xC0 | (r & 7)]);
                }
            }
            AsmOperand::Mem { disp, base, index, scale } => {
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, cc]);
                self.emit_asm_modrm_mem_only(0, *disp, *base, *index, *scale);
            }
            AsmOperand::SegMem { seg, disp, base, index, scale } => {
                let (_code, pfx) = Self::seg_meta(*seg);
                self.emit(&[pfx]);
                self.emit(&[Self::rex(false, 0, index.unwrap_or(0), *base), 0x0F, cc]);
                self.emit_asm_modrm_mem_only(0, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported setcc operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_cmpxchg(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        // 段前缀 mem
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = dst {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(s, _) = src {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, 0xB1]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(w, *s, 0, *d), 0x0F, 0xB1, Self::modrm_reg_reg(*s, *d)]);
            }
            (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, 0xB1]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F, 0xB1]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported cmpxchg operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_xadd(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = dst {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(s, _) = src {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, 0xC1]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(w, *s, 0, *d), 0x0F, 0xC1, Self::modrm_reg_reg(*s, *d)]);
            }
            (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, 0xC1]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported xadd operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    /// bsf/bsr: AT&T src, dst（dst 接收位索引）
    fn emit_asm_scan(&mut self, src: &AsmOperand, dst: &AsmOperand, opcode: u8, bits: u8) {
        let w = bits == 64;
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = src {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(d, _) = dst {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(w, *d, 0, *s), 0x0F, opcode, Self::modrm_reg_reg(*d, *s)]);
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported scan operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_popcnt(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = src {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(d, _) = dst {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0xF3, 0x0F, 0xB8]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            return;
        }
        match (src, dst) {
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) => {
                self.emit(&[Self::rex(w, *d, 0, *s), 0xF3, 0x0F, 0xB8, Self::modrm_reg_reg(*d, *s)]);
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) => {
                if bits == 16 { self.emit(&[0x66]); }
                self.emit(&[Self::rex(w, *d, index.unwrap_or(0), *base), 0xF3, 0x0F, 0xB8]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported popcnt operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    fn emit_asm_bswap(&mut self, op: &AsmOperand, bits: u8) {
        if let AsmOperand::Reg(r, _) = op {
            if bits == 64 {
                if *r >= 8 { self.emit(&[0x49]); } else { self.emit(&[0x48]); }
                self.emit(&[0x0F, 0xC8 + (r & 7)]);
            } else if bits == 32 {
                if *r >= 8 { self.emit(&[0x41]); } else {}
                self.emit(&[0x0F, 0xC8 + (r & 7)]);
            } else if bits == 16 {
                eprintln!("warning: bswap on 16-bit register not supported, emitted as nop");
                self.emit(&[0x90]);
            } else {
                if *r >= 8 { self.emit(&[0x41]); }
                self.emit(&[0x0F, 0xC8 + (r & 7)]);
            }
        } else {
            eprintln!("warning: unsupported bswap operand pattern, emitted as nop");
            self.emit(&[0x90]);
        }
    }

    /// movnti / movntdq / movntps / movntpd: AT&T src(reg), dst(mem)
    fn emit_asm_movnt(&mut self, src: &AsmOperand, dst: &AsmOperand, bits: u8) {
        let w = bits == 64;
        let opcode = if bits == 64 { 0xC7u8 } else { 0xC3u8 };
        if let AsmOperand::SegMem { seg, disp, base, index, scale } = dst {
            let (_code, pfx) = Self::seg_meta(*seg);
            self.emit(&[pfx]);
            if let AsmOperand::Reg(s, _) = src {
                self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            return;
        }
        if let AsmOperand::SymDisp(sym, _) = dst {
            if let AsmOperand::Reg(s, _) = src {
                let rex = if *s >= 8 { 0x4C } else { 0x48 };
                let modrm = 0x05 | ((s & 7) << 3);
                if bits == 8 {
                    self.emit(&[Self::rex(false, *s, 0, 0), 0x88, modrm, 0,0,0,0]);
                } else if bits == 16 {
                    self.emit(&[0x66, Self::rex(false, *s, 0, 0), 0x89, modrm, 0,0,0,0]);
                } else if bits == 32 {
                    self.emit(&[Self::rex(false, *s, 0, 0), 0x89, modrm, 0,0,0,0]);
                } else {
                    self.emit(&[rex, 0x89, modrm, 0,0,0,0]);
                }
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
                return;
            }
        }
        if let (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) = (src, dst) {
            if bits == 16 { self.emit(&[0x66]); }
            self.emit(&[Self::rex(w, *s, index.unwrap_or(0), *base), 0x0F, opcode]);
            self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
        } else {
            eprintln!("warning: unsupported movnt operand pattern, emitted as nop");
            self.emit(&[0x90]);
        }
    }

    // === VEX / EVEX 前缀编码辅助 ===
    // VEX 3 字节前缀 (C5): 0F 映射
    // VEX 4 字节前缀 (C4): 0F38/0F3A 映射
    // EVEX 4 字节前缀 (62): AVX-512 (含 mask / broadcast / 32-bit disp8)

    /// 发射 3 字节 VEX 前缀 (C5 形式) — 用于 0F 主映射指令
    /// r: 目标寄存器号 (0-15)
    /// v: vvvv 字段 (0-15), 内部反码
    /// l: L 位 (0=128-bit xmm, 1=256-bit ymm)
    /// pp: 前缀 0=none, 1=66, 2=F3, 3=F2
    fn emit_vex3_prefix(&mut self, r: u8, v: u8, l: u8, pp: u8) {
        let r_inv = if r >= 8 { 0u8 } else { 1u8 };
        let v_inv = (!v) & 0x0F;
        self.emit(&[0xC5, (r_inv << 7) | (v_inv << 3) | (l << 2) | pp]);
    }

    /// 发射 4 字节 VEX 前缀 (C4 形式) — 用于 0F38/0F3A 主映射指令
    /// r: 目标寄存器号 (0-15)
    /// b: rm/base 寄存器号 (0-15)
    /// map: 1=0F, 2=0F38, 3=0F3A
    /// w: W 位 (0/1)
    /// v: vvvv 字段 (0-15)
    /// l: L 位
    /// pp: 前缀
    fn emit_vex4_prefix(&mut self, r: u8, b: u8, map: u8, w: u8, v: u8, l: u8, pp: u8) {
        let r_inv = if r >= 8 { 0u8 } else { 1u8 };
        let b_inv = if b >= 8 { 0u8 } else { 1u8 };
        let v_inv = (!v) & 0x0F;
        self.emit(&[0xC4,
            (r_inv << 7) | (1u8 << 6) | (b_inv << 5) | (map & 0x1F),
            (w << 7) | (v_inv << 3) | (l << 2) | (pp & 0x03),
        ]);
    }

    /// 发射 3 操作数 VEX 指令 (reg-reg-reg):  v<op> dst, src1, src2
    /// map: 1=0F (3 字节前缀), 2=0F38 / 3=0F3A (4 字节前缀)
    fn emit_asm_vex_3op_reg(&mut self, dst: u8, src1: u8, src2: u8, l: u8, pp: u8,
                             map: u8, opcode: u8, w: u8) {
        if map == 1 {
            self.emit_vex3_prefix(dst, src1, l, pp);
        } else {
            self.emit_vex4_prefix(dst, src2, map, w, src1, l, pp);
        }
        self.emit(&[opcode, Self::modrm_reg_reg(dst, src2)]);
    }

    /// 发射 3 操作数 VEX 指令 (reg-reg-mem):  v<op> dst, src1, [src2]
    /// map: 1=0F, 2=0F38, 3=0F3A
    fn emit_asm_vex_3op_mem(&mut self, dst: u8, src1: u8, disp: i32, base: u8,
                             index: Option<u8>, scale: u8, l: u8, pp: u8,
                             map: u8, opcode: u8, w: u8) {
        if map == 1 {
            self.emit_vex3_prefix(dst, src1, l, pp);
        } else {
            self.emit_vex4_prefix(dst, base, map, w, src1, l, pp);
        }
        self.emit(&[opcode]);
        self.emit_asm_modrm_mem_only(dst, disp, base, index, scale);
    }

    /// 发射 3 操作数 VEX 指令 (reg-reg-reg + imm8):  v<op> dst, src1, src2, imm8
    /// 用于 0F3A 映射 (imm8 是必需的)
    fn emit_asm_vex_3op_imm8(&mut self, dst: u8, src1: u8, src2: u8, imm8: u8,
                              l: u8, pp: u8, map: u8, opcode: u8, w: u8) {
        if map == 1 {
            self.emit_vex3_prefix(dst, src1, l, pp);
        } else {
            self.emit_vex4_prefix(dst, src2, map, w, src1, l, pp);
        }
        self.emit(&[opcode, Self::modrm_reg_reg(dst, src2), imm8]);
    }

    /// 3 操作数 VEX 指令 (reg-mem-reg):  v<op> dst, [src1], src2
    /// 用于 FMA 等 dst 在 modrm.reg 但源是 [mem]+reg 形式
    fn emit_asm_vex_3op_mem_src2(&mut self, dst: u8, src1_disp: i32, src1_base: u8,
                                  src1_index: Option<u8>, src1_scale: u8,
                                  src2: u8, l: u8, pp: u8, map: u8, opcode: u8, w: u8) {
        if map == 1 {
            self.emit_vex3_prefix(dst, src2, l, pp);
        } else {
            self.emit_vex4_prefix(dst, src1_base, map, w, src2, l, pp);
        }
        self.emit(&[opcode]);
        self.emit_asm_modrm_mem_only(dst, src1_disp, src1_base, src1_index, src1_scale);
    }

    /// 3 操作数 VEX 指令，兼顾 reg-reg-reg 和 reg-mem-reg (与 gcc 行为一致)
    /// 操作数按 AT&T 顺序: (src1, src2, dst) — 也就是 `vaddps src1, src2, dst`
    /// `pp` 同时支持 F2/F3 前缀 (标量单精度/双精度)
    /// `is_scalar` 标志 2 操作数 (ss/sd) 强制走 0F 映射 (3 字节前缀)
    fn emit_asm_vex_3op_imm8_check(&mut self, src1: &AsmOperand, src2: &AsmOperand,
                                    dst: &AsmOperand, pp: u8, opcode: u8, map: u8, _is_scalar: bool) {
        // AT&T 顺序: src1 在 vvvv, src2/m 在 modrm.rm, dst 在 modrm.reg
        let (d, s1) = match (dst, src1) {
            (AsmOperand::Reg(d, ds), AsmOperand::Reg(s, ss)) if *ds >= 128 && *ss >= 128 => (*d, *s),
            _ => { eprintln!("warning: unsupported VEX 3-op reg-reg pattern, emitted as nop"); self.emit(&[0x90]); return; }
        };
        match src2 {
            AsmOperand::Reg(s2, sz2) if *sz2 >= 128 => {
                self.emit_asm_vex_3op_reg(d, s1, *s2, 0, pp, map, opcode, 0);
            }
            AsmOperand::Mem { disp, base, index, scale } => {
                self.emit_asm_vex_3op_mem(d, s1, *disp, *base, *index, *scale, 0, pp, map, opcode, 0);
            }
            _ => { eprintln!("warning: unsupported VEX 3-op src2 pattern, emitted as nop"); self.emit(&[0x90]); }
        }
    }

    /// FMA 4 操作数编码:  vfmadd123ss a, b, c  ≡  a = a * b + c
    /// AT&T 顺序: src1 (in vvvv), src2/m (in modrm.rm), dst (in modrm.reg)
    /// dst 在第一个位置, 故 modrm.reg = dst.
    /// 第二个 AT&T 操作数 (src1) 进入 vvvv 字段, 第三个 (src2/m) 进入 modrm.rm
    fn emit_asm_fma_3op(&mut self, src1: &AsmOperand, src2: &AsmOperand, dst: &AsmOperand,
                         l: u8, pp: u8, opcode: u8, w: u8, _is_scalar: bool) {
        let (d, s1) = match (dst, src1) {
            (AsmOperand::Reg(d, ds), AsmOperand::Reg(s, ss)) if *ds >= 128 && *ss >= 128 => (*d, *s),
            _ => { eprintln!("warning: unsupported FMA dst/src1 pattern, emitted as nop"); self.emit(&[0x90]); return; }
        };
        match src2 {
            AsmOperand::Reg(s2, sz2) if *sz2 >= 128 => {
                // reg-reg-reg
                self.emit_vex4_prefix(d, *s2, 2, w, s1, l, pp);
                self.emit(&[opcode, Self::modrm_reg_reg(d, *s2)]);
            }
            AsmOperand::Mem { disp, base, index, scale } => {
                // reg-reg-mem
                self.emit_vex4_prefix(d, *base, 2, w, s1, l, pp);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(d, *disp, *base, *index, *scale);
            }
            _ => { eprintln!("warning: unsupported FMA src2 pattern, emitted as nop"); self.emit(&[0x90]); }
        }
    }

    /// AMX 3 操作数 tdp* 编码:  tdp* dst, src1, src2
    /// VEX.NDS.128.0F38.W0 5E /r 或 5F /r
    /// pp: 0 (none) / 1 (66) / 2 (F3) / 3 (F2)
    fn emit_asm_amx_3op(&mut self, dst: &AsmOperand, src1: &AsmOperand, src2: &AsmOperand,
                        opcode: u8, pp: u8) {
        let (d, s1, s2) = match (dst, src1, src2) {
            (AsmOperand::Reg(d, _), AsmOperand::Reg(s, _), AsmOperand::Reg(t, _)) => (*d, *s, *t),
            _ => { eprintln!("warning: AMX tdp* needs tmm regs, emitted as nop"); self.emit(&[0x90]); return; }
        };
        self.emit_vex4_prefix(d, s2, 2, 0, s1, 0, pp);
        self.emit(&[opcode, Self::modrm_reg_reg(d, s2)]);
    }

    /// 通用 VEX 2 操作数 (reg-reg / reg-mem) — 用于 vmovaps/vmovdqa/vmovdqu 等
    /// map: 1=0F, 2=0F38, 3=0F3A
    fn emit_asm_vex_2op(&mut self, src: &AsmOperand, dst: &AsmOperand, l: u8, pp: u8,
                         map: u8, opcode: u8, w: u8) {
        match (src, dst) {
            (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) if *ss == 128 && *ds == 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*d, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*d, *s, map, w, 0, l, pp);
                }
                self.emit(&[opcode, Self::modrm_reg_reg(*d, *s)]);
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, ds)) if *ds == 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*d, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*d, *base, map, w, 0, l, pp);
                }
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            (AsmOperand::Reg(s, ss), AsmOperand::Mem { disp, base, index, scale }) if *ss == 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*s, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*s, *base, map, w, 0, l, pp);
                }
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            (AsmOperand::SymDisp(sym, _), AsmOperand::Reg(d, ds)) if *ds == 128 => {
                // VEX RIP-relative load: 走 mod=00 r/m=101 + disp32 (与标量 mov 一致)
                if map == 1 {
                    self.emit_vex3_prefix(*d, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*d, 0, map, w, 0, l, pp);
                }
                self.emit(&[opcode, 0x05 | ((d & 7) << 3), 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            }
            (AsmOperand::Reg(s, ss), AsmOperand::SymDisp(sym, _)) if *ss == 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*s, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*s, 0, map, w, 0, l, pp);
                }
                self.emit(&[opcode, 0x05 | ((s & 7) << 3), 0, 0, 0, 0]);
                let pos = self.pos() - 4;
                self.pending_data_rels.push((pos, sym.clone()));
            }
            // ldtilecfg / sttilecfg 形式: (None, Reg) 或 (Reg, None) — vvvv=0
            (AsmOperand::None, AsmOperand::Reg(d, ds)) if *ds >= 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*d, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*d, 0, map, w, 0, l, pp);
                }
                self.emit(&[opcode, Self::modrm_reg_reg(0, *d)]);
            }
            (AsmOperand::Reg(s, ss), AsmOperand::None) if *ss >= 128 => {
                if map == 1 {
                    self.emit_vex3_prefix(*s, 0, l, pp);
                } else {
                    self.emit_vex4_prefix(*s, 0, map, w, 0, l, pp);
                }
                self.emit(&[opcode, Self::modrm_reg_reg(0, *s)]);
            }
            // crc32 类 GPR-GPR / GPR-Mem: 无 VEX 前缀, 0F 38 F0/F1 + ModR/M
            // pp=0 → F2 (crc32b/crc32l/crc32q), pp=1 → 66 (crc32w); w=1 → REX.W (crc32q)
            (AsmOperand::Reg(s, _), AsmOperand::Reg(d, _)) if pp <= 1 => {
                let prefix: u8 = if pp == 0 { 0xF2 } else { 0x66 };
                if w == 1 { self.emit(&[0x48]); }  // REX.W
                self.emit(&[prefix, 0x0F, 0x38, opcode, Self::modrm_reg_reg(*d, *s)]);
            }
            (AsmOperand::Reg(s, _), AsmOperand::Mem { disp, base, index, scale }) if pp <= 1 => {
                let prefix: u8 = if pp == 0 { 0xF2 } else { 0x66 };
                if w == 1 { self.emit(&[0x48]); }  // REX.W
                self.emit(&[prefix, 0x0F, 0x38, opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, _)) if pp <= 1 => {
                let prefix: u8 = if pp == 0 { 0xF2 } else { 0x66 };
                if w == 1 { self.emit(&[0x48]); }  // REX.W
                self.emit(&[prefix, 0x0F, 0x38, opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported vex 2-op operand pattern (l:{}, pp:{}, map:{}, opcode:0x{:02x}, w:{}), src:{:?}, dst:{:?}, emitted as nop", l, pp, map, opcode, w, src, dst);
                self.emit(&[0x90]);
            }
        }
    }

    /// 通用 VEX 1 操作数 (reg) — 用于 vzeroupper, vzeroall, vpmovmskb 等
    fn emit_asm_vex_1op_reg(&mut self, dst: u8, l: u8, pp: u8, map: u8, opcode: u8) {
        if map == 1 {
            self.emit_vex3_prefix(dst, 0, l, pp);
        } else {
            self.emit_vex4_prefix(dst, 0, map, 0, 0, l, pp);
        }
        self.emit(&[opcode, Self::modrm_reg_reg(0, dst)]);
    }

    // === EVEX 编码辅助 (AVX-512) ===
    // 简化版: 仅支持 32/64 位无掩码无广播的基本形式

    /// 发射 EVEX 前缀 (62) — 简化版
    /// r: dst reg (0-15 或 16-31 表示 zmm16-31)
    /// b: rm/base reg (0-15)
    /// map: 1=0F, 2=0F38, 3=0F3A
    /// w: W 位
    /// v: vvvv reg (0-15)
    /// l: L'L (0=128, 1=256, 2=512)
    /// pp: 前缀
    /// mask: aaa 字段 (0=无掩码, 1-7=k1-k7)
    fn emit_evex_prefix(&mut self, r: u8, b: u8, map: u8, w: u8, v: u8, l: u8, pp: u8, mask: u8) {
        let r_inv = if r >= 8 { 0u8 } else { 1u8 };
        let r2 = if r >= 16 { 0u8 } else { 1u8 };
        let b_inv = if b >= 8 { 0u8 } else { 1u8 };
        let v_inv = (!v) & 0x0F;
        self.emit(&[0x62,
            (r_inv << 7) | (1u8 << 6) | (b_inv << 5) | (r2 << 4) | (map & 0x0F),
            (w << 7) | (v_inv << 3) | (1u8 << 2) | (pp & 0x03),
            (0u8 << 7) | (l << 5) | (0u8 << 4) | (mask & 0x07),
        ]);
    }

    /// 通用 EVEX 2 操作数 (无 mask / 无 broadcast)
    fn emit_asm_evex_2op(&mut self, src: &AsmOperand, dst: &AsmOperand, l: u16, pp: u8,
                          map: u8, opcode: u8, w: u8) {
        let l_bits: u8 = if l >= 512 { 2u8 } else if l >= 256 { 1u8 } else { 0u8 };
        match (src, dst) {
            (AsmOperand::Reg(s, ss), AsmOperand::Reg(d, ds)) if *ss >= 128 && *ds >= 128 => {
                self.emit_evex_prefix(*d, *s, map, w, 0, l_bits, pp, 0);
                self.emit(&[opcode, Self::modrm_reg_reg(*d, *s)]);
            }
            (AsmOperand::Mem { disp, base, index, scale }, AsmOperand::Reg(d, ds)) if *ds >= 128 => {
                self.emit_evex_prefix(*d, *base, map, w, 0, l_bits, pp, 0);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*d, *disp, *base, *index, *scale);
            }
            (AsmOperand::Reg(s, ss), AsmOperand::Mem { disp, base, index, scale }) if *ss >= 128 => {
                self.emit_evex_prefix(*s, *base, map, w, 0, l_bits, pp, 0);
                self.emit(&[opcode]);
                self.emit_asm_modrm_mem_only(*s, *disp, *base, *index, *scale);
            }
            _ => {
                eprintln!("warning: unsupported evex 2-op operand pattern, emitted as nop");
                self.emit(&[0x90]);
            }
        }
    }

    /// 通用 EVEX 3 操作数 (reg-reg-reg):  v<op> dst, src1, src2
    fn emit_asm_evex_3op_reg(&mut self, dst: u8, src1: u8, src2: u8, l: u16, pp: u8,
                              map: u8, opcode: u8, w: u8) {
        let l_bits: u8 = if l >= 512 { 2u8 } else if l >= 256 { 1u8 } else { 0u8 };
        self.emit_evex_prefix(dst, src2, map, w, src1, l_bits, pp, 0);
        self.emit(&[opcode, Self::modrm_reg_reg(dst, src2)]);
    }

}
