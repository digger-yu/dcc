// Helper type definitions for x86_64 backend
// Extracted from mod.rs as part of Path A Backend Refactor

use crate::codegen::shared::elf::{ComdatGroup, ElfRela, ElfSection, ExtraRelaSection, ElfSym};
use crate::preproc::LineSpan;
use crate::target::Target;
use std::collections::{HashMap, HashSet};

/// Per-function layout captured after resolve (for -S disassembly).
#[derive(Clone)]
pub struct FuncAsmLayout {
    pub section: String,
    pub bytes: Vec<u8>,
    pub text_start: u64,
}
/// Per-function layout captured after resolve (for -S disassembly).
struct FuncAsmLayout {
    section: String,
    bytes: Vec<u8>,
    text_start: u64,
}

#[derive(Clone)]
struct PendingJump {
    /// 原始 self.code 中的指令起始偏移。
    at: u64,
    target: String,
    kind: JumpKind,
    /// disp32/imm32 字段在原始 self.code 中的偏移。
    disp_offset: u64,
    /// PC-relative 指令的下一条指令偏移。
    next_ip: u64,
    /// push $label: imm32 = absolute address (not PC-relative)
    is_push_imm: bool,
}
#[derive(Clone, Copy, PartialEq)]
pub enum JumpKind {
    Jmp,
    Je,
    Jne,
    Jl,
    Jle,
    Jg,
    Jge,
    Jb,
    Ja,
    Jns,
    Jo,
}
#[derive(Clone)]
struct GlobalVar {
    offset: u64,
    size: usize,
    ty: Type,
    init: Option<Vec<u8>>,
    section: u16,
    is_tls: bool,
    in_data_section: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbiClass {
    NoClass,
    Integer,
    Sse,
    SseUp,
    X87,
    X87Up,
    ComplexX87,
    Memory,
}

#[derive(Clone, Copy)]
pub enum ArgLoc {
    Gp(u8),
    Xmm(u8),
    Stack(usize),
}

/// va_list 寻址模式: 统一表示 va_arg 的各种 va_list 来源.
/// 简化 gen_va_arg_* 函数族的重载分发.
#[derive(Clone, Debug)]
enum VaListAddress {
    /// 命名变量 (局部或全局). codegen 内部按 is_global_not_local 分派.
    Named(String),
    /// va_list 地址已在寄存器 rcx 中 (来自指针解引用或参数退化).
    InReg,
    /// va_list 地址在 [r13 + offset] (来自结构体成员或数组下标).
    Lvalue(i32),
}

/// 内联汇编操作数
#[derive(Clone, Debug)]
enum AsmOperand {
    None,
    Reg(u8, u8), // (reg_index, size_bits)
    Imm(i64),    // 立即数
    Mem {
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
    },
    Sym(String), // 符号/标签名
    /// 段寄存器: 0=ES, 1=CS, 2=SS, 3=DS, 4=FS, 5=GS
    Seg(u8),
    /// 段前缀 + mem: 用于 `mov %gs:offset(%rip), %rax` 等
    SegMem {
        seg: u8,
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
    },
    /// 全局符号 + RIP-relative (sym@PLT / 直接 PC-relative)
    SymDisp(String, i32),
}

/// parse_asm_mem 中 %rip 相对寻址的 base 哨兵值
const ASM_BASE_RIP: u8 = 255;
/// mini-asm 寄存器宽度标签（u8 编码，>=128 表示向量/特殊寄存器）
const ASM_REG_XMM: u8 = 128;
const ASM_REG_YMM: u8 = 129;
const ASM_REG_ZMM: u8 = 130;
const ASM_REG_TMM: u8 = 131;
const ASM_REG_K: u8 = 132;
/// mini-asm 特殊寄存器宽度标签: 控制寄存器 (crN) / 调试寄存器 (drN, dbN)
const ASM_REG_MMX: u8 = 135;
const ASM_REG_ST: u8 = 136; // x87 FPU 栈寄存器 st(0)-st(7)
const ASM_REG_CR: u8 = 133;
const ASM_REG_DR: u8 = 134;

/// 内联 asm 约束绑定结果
struct AsmOpBinding {
    /// 分配到的物理寄存器索引 (rax=0 …)
    /// 对于 @cc 约束: 存储条件码字母的 ASCII 值
    /// 对于段寄存器: 存储段码 (0=ES,1=CS,2=SS,3=DS,4=FS,5=GS)
    /// 对于 MMX: 存储 mm 寄存器索引 (0-7)
    reg: u8,
    /// 是否为输出/读写操作数
    is_output: bool,
    /// 模板替换为内存操作数
    is_mem: bool,
    /// early-clobber (`&`): 分配到的寄存器与所有输入都不重叠
    early_clobber: bool,
    /// 寄存器类型/宽度编码:
    ///   0 = GP (64 位), 32 = XMM float, 64 = XMM double, 128 = XMM full
    ///   200 = x87 st(0), 201 = x87 st(1)
    ///   202 = MMX (reg 字段为 mm 索引)
    ///   203 = 段寄存器 (reg 字段为段码)
    ///   204 = 条件码 (reg 字段为 cc ASCII 字母)
    ///   205-208 = 显式子寄存器 (reg 字段为 GP 索引, 205=8b 206=16b 207=32b 208=64b)
    xmm_width: u8,
}

/// 显式寄存器约束解析结果
#[derive(Clone, Copy)]
enum ExplicitRegKind {
    Gp,
    Seg,
}

struct ExplicitRegInfo {
    /// 寄存器索引 (GP: rax=0…; Seg: 0=ES,1=CS,…,4=FS,5=GS)
    reg: u8,
    /// 寄存器宽度 (bits): 8/16/32/64
    size: u8,
    /// AT&T 语法寄存器名 (不含 % 前缀, 如 "eax", "cx", "gs")
    att_name: &'static str,
    /// 寄存器种类
    kind: ExplicitRegKind,
}

#[derive(Clone, Copy)]
enum AsmRegUse {
    Input,
    Output,
}

struct AsmRegisterPlanner {
    used_gp: HashSet<u8>,
    used_xmm: HashSet<u8>,
    used_mmx: HashSet<u8>,
    input_gp: HashSet<u8>,
    input_xmm: HashSet<u8>,
    clobber_gp: HashSet<u8>,
    clobber_xmm: HashSet<u8>,
}

impl AsmRegisterPlanner {
    const GP_POOL: [u8; 9] = [0, 1, 2, 6, 7, 8, 9, 10, 11];
    const GP_LEGACY_POOL: [u8; 4] = [0, 3, 1, 2];
    const XMM_POOL: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    const MMX_POOL: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

    fn new(clobber_gp: &HashSet<u8>, clobber_xmm: &HashSet<u8>) -> Self {
        Self {
            used_gp: clobber_gp.clone(),
            used_xmm: clobber_xmm.clone(),
            used_mmx: HashSet::new(),
            input_gp: HashSet::new(),
            input_xmm: HashSet::new(),
            clobber_gp: clobber_gp.clone(),
            clobber_xmm: clobber_xmm.clone(),
        }
    }

    fn alloc_gp(&mut self, seed: usize, use_kind: AsmRegUse) -> u8 {
        let reg = Self::pick(&Self::GP_POOL, &self.used_gp, &self.clobber_gp, seed, None)
            .unwrap_or(Self::GP_POOL[seed % Self::GP_POOL.len()]);
        self.reserve_gp(reg, use_kind);
        reg
    }

    fn alloc_gp_for_constraint(
        &mut self,
        constraint: &str,
        seed: usize,
        use_kind: AsmRegUse,
    ) -> Option<(u8, bool)> {
        let core = constraint.trim_start_matches(['=', '+', '&', '%']);
        if core.contains('{') || core.starts_with("@cc") {
            return None;
        }
        let letters: Vec<char> = core.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        if letters.iter().any(|c| matches!(c, 'r' | 'R' | 'g')) {
            return None;
        }
        let fixed = if letters.contains(&'a') {
            Some(0)
        } else if letters.contains(&'b') {
            Some(3)
        } else if letters.contains(&'c') {
            Some(1)
        } else if letters.contains(&'d') {
            Some(2)
        } else if letters.contains(&'S') {
            Some(6)
        } else if letters.contains(&'D') {
            Some(7)
        } else {
            None
        };
        if let Some(reg) = fixed {
            self.reserve_gp(reg, use_kind);
            return Some((reg, true));
        }
        if letters.iter().any(|c| matches!(c, 'q' | 'Q')) {
            let reg = Self::pick(
                &Self::GP_LEGACY_POOL,
                &self.used_gp,
                &self.clobber_gp,
                seed,
                None,
            )
            .unwrap_or(Self::GP_LEGACY_POOL[seed % Self::GP_LEGACY_POOL.len()]);
            self.reserve_gp(reg, use_kind);
            return Some((reg, false));
        }
        None
    }

    fn alloc_xmm(&mut self, seed: usize, use_kind: AsmRegUse) -> u8 {
        let reg = Self::pick(
            &Self::XMM_POOL,
            &self.used_xmm,
            &self.clobber_xmm,
            seed,
            None,
        )
        .unwrap_or(Self::XMM_POOL[seed % Self::XMM_POOL.len()]);
        self.reserve_xmm(reg, use_kind);
        reg
    }

    fn alloc_mmx(&mut self, seed: usize) -> u8 {
        let reg = Self::pick(&Self::MMX_POOL, &self.used_mmx, &HashSet::new(), seed, None)
            .unwrap_or(Self::MMX_POOL[seed % Self::MMX_POOL.len()]);
        self.used_mmx.insert(reg);
        reg
    }

    fn reserve_gp(&mut self, reg: u8, use_kind: AsmRegUse) {
        self.used_gp.insert(reg);
        if matches!(use_kind, AsmRegUse::Input) {
            self.input_gp.insert(reg);
        }
    }

    fn reserve_xmm(&mut self, reg: u8, use_kind: AsmRegUse) {
        self.used_xmm.insert(reg);
        if matches!(use_kind, AsmRegUse::Input) {
            self.input_xmm.insert(reg);
        }
    }

    fn input_gp_contains(&self, reg: u8) -> bool {
        self.input_gp.contains(&reg)
    }

    fn input_xmm_contains(&self, reg: u8) -> bool {
        self.input_xmm.contains(&reg)
    }

    fn reassign_early_gp(&mut self, old: u8, seed: usize) -> Option<u8> {
        if !self.input_gp.contains(&old) && !self.clobber_gp.contains(&old) {
            self.used_gp.remove(&old);
        }
        let reg = Self::pick(
            &Self::GP_POOL,
            &self.used_gp,
            &self.clobber_gp,
            seed,
            Some(&self.input_gp),
        )?;
        self.used_gp.insert(reg);
        Some(reg)
    }

    fn reassign_early_xmm(&mut self, old: u8, seed: usize) -> Option<u8> {
        if !self.input_xmm.contains(&old) && !self.clobber_xmm.contains(&old) {
            self.used_xmm.remove(&old);
        }
        let reg = Self::pick(
            &Self::XMM_POOL,
            &self.used_xmm,
            &self.clobber_xmm,
            seed,
            Some(&self.input_xmm),
        )?;
        self.used_xmm.insert(reg);
        Some(reg)
    }

    fn pick(
        pool: &[u8],
        used: &HashSet<u8>,
        clobbered: &HashSet<u8>,
        seed: usize,
        avoid: Option<&HashSet<u8>>,
    ) -> Option<u8> {
        for step in 0..pool.len() {
            let reg = pool[(seed + step) % pool.len()];
            if used.contains(&reg) || clobbered.contains(&reg) {
                continue;
            }
            if avoid.is_some_and(|set| set.contains(&reg)) {
                continue;
            }
            return Some(reg);
        }
        for &reg in pool {
            if clobbered.contains(&reg) || avoid.is_some_and(|set| set.contains(&reg)) {
                continue;
            }
            return Some(reg);
        }
        None
    }
}

/// .pushsection 非文本节数据收集: 待解析的符号引用重定位
#[derive(Clone)]
struct CustomSecRel {
    sec_idx: usize,
    offset: u64,
    symbol: String,
    rel_type: u32,
    addend: i64,
}

/// 延迟解析: pushsection 数据中两个标签差值表达式 (前向引用)
#[derive(Clone)]
struct DeferredPushsectionFixup {
    sec_idx: usize,
    data_offset: usize,
    left_label: String,
    right_label: String,
    byte_size: usize, // 1 for .byte, 2 for .short
}

/// .text / executable pushsection 内数据伪指令产生的延迟重定位。
#[derive(Clone)]
struct PendingCodeRel {
    offset: u64,
    symbol: String,
    rel_type: u32,
    addend: i64,
}

/// GNU as 局部数字标签：同一块 asm 内 `6:` 定义，`6f`/`6b` 引用。
struct AsmLocalLabels {
    /// (行号, 标签号, 唯一内部名)
    defs: Vec<(usize, u32, String)>,
}

/// `parse_asm_data_expr` 的求值结果:
/// (数值, 符号引用 (symbol, rel_type, addend), 延迟标签差值 (left, right))
type AsmDataExprValue = (i64, Option<(String, u32, i64)>, Option<(String, String)>);

impl AsmLocalLabels {
    fn scan(lines: &[String], scope: u32) -> Self {
        let mut defs = Vec::new();
        let mut serial: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if let Some(num) = Self::parse_numeric_label(t) {
                let n = serial.entry(num).or_insert(0);
                let name = format!(".Lasm_local_{scope}_{num}_{n}");
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
            } else {
                None
            };
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
                .rfind(|(i, n, _)| *i < at_line && *n == num)
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
                        let num: u32 = chars[start..i]
                            .iter()
                            .collect::<String>()
                            .parse()
                            .unwrap_or(0);
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

/// GCC asm 高级指令展开器: .rept/.endr/.irp/.irpc/.macro/.endm/.if/.else/.endif/.ifdef/.ifndef/.include/.intel_syntax/.att_syntax/.altmacro
///
/// 在 asm 模板行进入 mini-assembler 之前, 先做文本级预处理:
/// - `.rept N` / `.endr`: 重复块 N 次
/// - `.irp sym, v1, v2, ...` / `.endr`: 对每个值替换 sym 后展开块
/// - `.irpc sym, chars` / `.endr`: 对每个字符替换 sym 后展开块
/// - `.macro name [args]` / `.endm`: 定义宏; 后续行中 `name [args]` 调用时展开
/// - `.include "file"`: 读入文件内容 (相对于源文件目录或 cwd)
/// - `.if EXPR` / `.elseif EXPR` / `.else` / `.endif`: 条件汇编 (简单整数表达式)
/// - `.ifdef SYM` / `.ifndef SYM`: 符号定义条件 (始终为 false, 因 asm 内无 .set 之外的符号表)
/// - `.intel_syntax noprefix` / `.att_syntax`: 语法切换 (跟踪状态, 透传至 -S 输出)
/// - `.altmacro` / `.noaltmacro`: 交替宏模式 (跟踪状态, 影响 % 参数前缀)
struct AsmDirectiveExpander {
    /// 已定义宏: name -> (参数名列表, 行列表)
    macros: std::collections::HashMap<String, (Vec<String>, Vec<String>)>,
    /// 是否处于 Intel 语法模式
    intel_syntax: bool,
    /// 是否处于 .altmacro 模式
    alt_macro: bool,
}

impl AsmDirectiveExpander {
    fn new() -> Self {
        Self {
            macros: std::collections::HashMap::new(),
            intel_syntax: false,
            alt_macro: false,
        }
    }

    /// 展开 asm 字符串中的所有高级指令, 返回纯指令行列表
    fn expand(&mut self, lines: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].trim();
            if line.is_empty() {
                i += 1;
                continue;
            }
            // .rept N ... .endr
            if let Some(count) = Self::parse_rept(line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endr");
                let expanded_body = self.expand(&body);
                for _ in 0..count {
                    out.extend(expanded_body.clone());
                }
                i = end_idx + 1;
                continue;
            }
            // .irp sym, v1, v2, ... ... .endr
            if let Some((sym, vals)) = Self::parse_irp(line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endr");
                let expanded_body = self.expand(&body);
                for val in &vals {
                    for l in &expanded_body {
                        out.push(
                            l.replace(&format!("\\{sym}"), val)
                                .replace(&format!("\\({sym})"), val),
                        );
                    }
                }
                i = end_idx + 1;
                continue;
            }
            // .irpc sym, chars ... .endr
            if let Some((sym, chars)) = Self::parse_irpc(line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endr");
                let expanded_body = self.expand(&body);
                for c in chars.chars() {
                    for l in &expanded_body {
                        out.push(
                            l.replace(&format!("\\{sym}"), &c.to_string())
                                .replace(&format!("\\({sym})"), &c.to_string()),
                        );
                    }
                }
                i = end_idx + 1;
                continue;
            }
            // .macro name [args] ... .endm
            if let Some((name, args)) = Self::parse_macro_def(line) {
                let (body, end_idx) = Self::collect_block(lines, i + 1, ".endm");
                self.macros.insert(name.clone(), (args.clone(), body));
                i = end_idx + 1;
                continue;
            }
            // 宏调用: name [args]
            if let Some(expanded) = self.try_expand_macro_call(line) {
                out.extend(expanded);
                i += 1;
                continue;
            }
            // .include "file"
            if let Some(path) = Self::parse_include(line) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let inc_lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
                    let expanded = self.expand(&inc_lines);
                    out.extend(expanded);
                } else {
                    // 文件不存在, 静默跳过 (GCC 兼容: .include 失败不报错)
                }
                i += 1;
                continue;
            }
            // .if / .ifdef / .ifndef ... .else / .elseif ... .endif
            if let Some(cond_result) = Self::parse_conditional(line) {
                let (body, end_idx) = Self::collect_conditional_block(lines, i + 1, cond_result);
                let expanded = self.expand(&body);
                out.extend(expanded);
                i = end_idx + 1;
                continue;
            }
            // .intel_syntax noprefix / .att_syntax
            if line == ".intel_syntax noprefix" || line == ".intel_syntax" {
                self.intel_syntax = true;
                out.push(line.to_string());
                i += 1;
                continue;
            }
            if line == ".att_syntax" || line == ".att_syntax prefix" {
                self.intel_syntax = false;
                out.push(line.to_string());
                i += 1;
                continue;
            }
            // .altmacro / .noaltmacro
            if line == ".altmacro" {
                self.alt_macro = true;
                out.push(line.to_string());
                i += 1;
                continue;
            }
            if line == ".noaltmacro" {
                self.alt_macro = false;
                out.push(line.to_string());
                i += 1;
                continue;
            }
            // 普通行: 直接传递
            out.push(line.to_string());
            i += 1;
        }
        out
    }

    /// 解析 `.rept N` 返回重复次数
    fn parse_rept(line: &str) -> Option<usize> {
        let t = line.trim();
        let rest = t.strip_prefix(".rept")?;
        let rest = rest.trim();
        rest.split_whitespace().next()?.parse().ok()
    }

    /// 解析 `.irp sym, v1, v2, ...` 返回 (符号名, 值列表)
    fn parse_irp(line: &str) -> Option<(String, Vec<String>)> {
        let t = line.trim();
        let rest = t.strip_prefix(".irp")?;
        let rest = rest.trim();
        let mut parts = rest.splitn(2, ',');
        let sym = parts.next()?.trim().to_string();
        let vals_str = parts.next()?;
        let vals: Vec<String> = vals_str.split(',').map(|s| s.trim().to_string()).collect();
        Some((sym, vals))
    }

    /// 解析 `.irpc sym, chars` 返回 (符号名, 字符序列)
    fn parse_irpc(line: &str) -> Option<(String, String)> {
        let t = line.trim();
        let rest = t.strip_prefix(".irpc")?;
        let rest = rest.trim();
        let mut parts = rest.splitn(2, ',');
        let sym = parts.next()?.trim().to_string();
        let chars = parts.next()?.trim().to_string();
        Some((sym, chars))
    }

    /// 解析 `.macro name [arg1, arg2, ...]` 返回 (宏名, 参数名列表)
    fn parse_macro_def(line: &str) -> Option<(String, Vec<String>)> {
        let t = line.trim();
        let rest = t.strip_prefix(".macro")?;
        let rest = rest.trim();
        // 收集宏名后的所有剩余文本
        let mut parts = rest.splitn(2, |c: char| c.is_whitespace());
        let name = parts.next()?.trim().to_string();
        let args_rest = parts.next().unwrap_or("").trim();
        let args: Vec<String> = if args_rest.is_empty() {
            Vec::new()
        } else {
            args_rest
                .split(',')
                .map(|s| s.trim().trim_start_matches('\\').to_string())
                .collect()
        };
        Some((name, args))
    }

    /// 尝试展开宏调用, 如果不是宏调用返回 None
    fn try_expand_macro_call(&self, line: &str) -> Option<Vec<String>> {
        let t = line.trim();
        if t.starts_with('.') {
            return None;
        }
        let mut parts = t.splitn(2, |c: char| c.is_whitespace());
        let name = parts.next()?.trim();
        if let Some((args_def, body)) = self.macros.get(name) {
            let rest = parts.next().unwrap_or("").trim();
            let call_args: Vec<String> = if rest.is_empty() {
                Vec::new()
            } else {
                rest.split(',').map(|s| s.trim().to_string()).collect()
            };
            let mut result = Vec::new();
            for l in body {
                let mut expanded = l.clone();
                for (i, arg_name) in args_def.iter().enumerate() {
                    let val = call_args.get(i).cloned().unwrap_or_default();
                    expanded = expanded.replace(&format!("\\{arg_name}"), &val);
                    expanded = expanded.replace(&format!("\\({arg_name})"), &val);
                }
                result.push(expanded);
            }
            Some(result)
        } else {
            None
        }
    }

    /// 解析 `.include "file"` 返回文件路径
    fn parse_include(line: &str) -> Option<String> {
        let t = line.trim();
        let rest = t.strip_prefix(".include")?;
        let rest = rest.trim();
        let path = rest.trim_matches('"').to_string();
        Some(path)
    }

    /// 解析条件指令, 返回条件结果 (true = 包含块, false = 跳过块)
    fn parse_conditional(line: &str) -> Option<bool> {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(".ifdef") {
            // .ifdef: 始终 false (asm 内无符号表)
            let _ = rest.trim();
            return Some(false);
        }
        if let Some(rest) = t.strip_prefix(".ifndef") {
            // .ifndef: 始终 true
            let _ = rest.trim();
            return Some(true);
        }
        if let Some(rest) = t.strip_prefix(".if") {
            let rest = rest.trim();
            // 简单整数表达式求值
            return Some(Self::eval_simple_expr(rest));
        }
        None
    }

    /// 简单整数表达式求值 (支持: 数字, 0x前缀, +, -, *, /, |, &, ^, !, ==, !=, <, >, <=, >=)
    fn eval_simple_expr(s: &str) -> bool {
        let s = s.trim();
        // 尝试解析为整数
        if let Ok(v) = Self::eval_expr_value(s) {
            return v != 0;
        }
        // 无法解析, 默认 false
        false
    }

    /// 求值整数表达式 (简化版, 支持基本运算)
    fn eval_expr_value(s: &str) -> Result<i64, ()> {
        let s = s.trim().trim_end_matches(',');
        // 处理括号
        if s.starts_with('(') && s.ends_with(')') {
            return Self::eval_expr_value(&s[1..s.len() - 1]);
        }
        // 处理一元 !
        if let Some(rest) = s.strip_prefix('!') {
            let v = Self::eval_expr_value(rest)?;
            return Ok(if v == 0 { 1 } else { 0 });
        }
        // 处理一元 -
        if let Some(rest) = s.strip_prefix('-') {
            if !rest.is_empty() && !rest.contains(['+', '-', '*', '/']) {
                let v = Self::eval_expr_value(rest)?;
                return Ok(-v);
            }
        }
        // 处理比较运算符 (低优先级)
        for op in &["==", "!=", "<=", ">=", "<", ">"] {
            if let Some(pos) = s.find(op) {
                let left = Self::eval_expr_value(&s[..pos])?;
                let right = Self::eval_expr_value(&s[pos + op.len()..])?;
                return Ok(match *op {
                    "==" => {
                        if left == right {
                            1
                        } else {
                            0
                        }
                    }
                    "!=" => {
                        if left != right {
                            1
                        } else {
                            0
                        }
                    }
                    "<=" => {
                        if left <= right {
                            1
                        } else {
                            0
                        }
                    }
                    ">=" => {
                        if left >= right {
                            1
                        } else {
                            0
                        }
                    }
                    "<" => {
                        if left < right {
                            1
                        } else {
                            0
                        }
                    }
                    ">" => {
                        if left > right {
                            1
                        } else {
                            0
                        }
                    }
                    _ => return Err(()),
                });
            }
        }
        // 处理 | & ^ (低优先级)
        for op in &['|', '&', '^'] {
            if let Some(pos) = s.rfind(*op) {
                let left = Self::eval_expr_value(&s[..pos])?;
                let right = Self::eval_expr_value(&s[pos + 1..])?;
                return Ok(match op {
                    '|' => left | right,
                    '&' => left & right,
                    '^' => left ^ right,
                    _ => return Err(()),
                });
            }
        }
        // 处理 + - (中优先级)
        for op in &['+', '-'] {
            if let Some(pos) = s.rfind(*op) {
                if pos > 0 {
                    let left = Self::eval_expr_value(&s[..pos])?;
                    let right = Self::eval_expr_value(&s[pos + 1..])?;
                    return Ok(match op {
                        '+' => left + right,
                        '-' => left - right,
                        _ => return Err(()),
                    });
                }
            }
        }
        // 处理 * / (高优先级)
        for op in &['*', '/'] {
            if let Some(pos) = s.rfind(*op) {
                if pos == 0 {
                    continue;
                }
                let left = Self::eval_expr_value(&s[..pos])?;
                let right = Self::eval_expr_value(&s[pos + 1..])?;
                return Ok(match op {
                    '*' => left * right,
                    '/' if right != 0 => left / right,
                    _ => return Err(()),
                });
            }
        }
        // 基本数字
        Self::parse_number(s)
    }

    /// 解析数字 (支持 0x 前缀, 0b 前缀, 普通十进制)
    fn parse_number(s: &str) -> Result<i64, ()> {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            i64::from_str_radix(hex, 16).map_err(|_| ())
        } else if let Some(bin) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
            i64::from_str_radix(bin, 2).map_err(|_| ())
        } else {
            s.parse().map_err(|_| ())
        }
    }

    /// 收集块直到遇到 end_directive (处理嵌套)
    fn collect_block(lines: &[String], start: usize, end_directive: &str) -> (Vec<String>, usize) {
        let mut body = Vec::new();
        let mut depth = 1;
        let mut i = start;
        while i < lines.len() {
            let t = lines[i].trim();
            // 检查嵌套开始
            if t.starts_with(".rept")
                || t.starts_with(".irp")
                || t.starts_with(".irpc")
                || t.starts_with(".macro")
                || t.starts_with(".if")
                || t.starts_with(".ifdef")
                || t.starts_with(".ifndef")
            {
                depth += 1;
                body.push(lines[i].clone());
            } else if t == end_directive || t.starts_with(&format!("{end_directive} ")) {
                depth -= 1;
                if depth == 0 {
                    return (body, i);
                }
                body.push(lines[i].clone());
            } else {
                body.push(lines[i].clone());
            }
            i += 1;
        }
        (body, lines.len())
    }

    /// 收集条件块 (.if/.elseif/.else/.endif), 返回 (选中块内容, endif 后行号)
    fn collect_conditional_block(
        lines: &[String],
        start: usize,
        first_cond: bool,
    ) -> (Vec<String>, usize) {
        let mut current_branch = Vec::new();
        let mut selected_branch: Option<Vec<String>> = None;
        let mut current_is_true = first_cond;
        let mut depth = 1;
        let mut i = start;
        while i < lines.len() {
            let t = lines[i].trim();
            if t.starts_with(".if") || t.starts_with(".ifdef") || t.starts_with(".ifndef") {
                depth += 1;
                if depth == 2 {
                    current_branch.push(lines[i].clone());
                }
            } else if t == ".endif" || t.starts_with(".endif ") {
                depth -= 1;
                if depth == 0 {
                    // 结束: 如果还没选中且当前分支为 true, 选当前
                    if selected_branch.is_none() && current_is_true {
                        selected_branch = Some(current_branch);
                    }
                    return (selected_branch.unwrap_or_default(), i);
                }
                if depth == 1 {
                    current_branch.push(lines[i].clone());
                }
            } else if depth == 1 && (t == ".else" || t.starts_with(".else ")) {
                // 如果当前分支为 true 且还没选中, 选中它
                if current_is_true && selected_branch.is_none() {
                    selected_branch = Some(current_branch);
                }
                current_branch = Vec::new();
                // .else 分支: 只有前面都没选中才可能为 true
                current_is_true = selected_branch.is_none();
            } else if depth == 1 && t.starts_with(".elseif") {
                if current_is_true && selected_branch.is_none() {
                    selected_branch = Some(current_branch);
                }
                current_branch = Vec::new();
                // .elseif 条件暂不处理, 简化为 false
                current_is_true = false;
            } else {
                if depth == 1 {
                    current_branch.push(lines[i].clone());
                }
            }
            i += 1;
        }
        // 未找到 .endif, 返回已收集的内容
        if selected_branch.is_none() && current_is_true {
            (current_branch, i)
        } else {
            (selected_branch.unwrap_or_default(), i)
        }
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
    line_map: Vec<LineSpan>,
    globals: HashMap<String, ParallelGlobalSnap>,
    variadic_funcs: HashMap<String, bool>,
    old_style_funcs: std::collections::HashSet<String>,
    nested_funcs: HashMap<String, Vec<(String, i64)>>,
    nested_func_renames: HashMap<String, HashMap<String, String>>,
    nested_func_parent: HashMap<String, String>,
    nested_tramp_offsets: HashMap<String, i64>,
    nested_chain_offsets: HashMap<String, i64>,
    nested_func_rename_counter: u32,
    func_param_types: HashMap<String, Vec<Type>>,
    defined_funcs: HashSet<String>,
    error_attr_fns: HashSet<String>,
    global_reg_bindings: HashMap<String, u8>,
    rodata: Vec<u8>,
    string_pool: HashMap<Vec<u8>, u64>,
    text_align: u64,
}

#[derive(Clone)]
struct FuncEmitChunk {
    name: String,
    code: Vec<u8>,
    entry_offset: u64,
    size: u64,
    labels: HashMap<String, u64>,
    pending: Vec<PendingJump>,
    string_refs: Vec<(u64, Vec<u8>)>,
    float_refs: Vec<(u64, u64)>,
    x87_float_refs: Vec<(u64, u64)>,
    pending_func_rels: Vec<(u64, String)>,
    pending_code_rels: Vec<PendingCodeRel>,
    pending_func_addr_rels: Vec<(u64, String, bool)>,
    pending_data_rels: Vec<(u64, String)>,
    pending_sib_data_rels: Vec<(u64, String)>,
    label_addr_refs: Vec<(u64, String)>,
    pending_custom_sec_rels: Vec<CustomSecRel>,
    pushsection_code_ranges: Vec<(u64, u64, usize, u64)>,
    extra_sections: Vec<elf::ElfSection>,
    custom_sec_indices: HashMap<String, usize>,
    disasm_calls: HashMap<u64, String>,
    /// rodata 增量: chunk 编译期间新增的 rodata 数据
    new_rodata: Vec<u8>,
    /// chunk 开始时的 rodata 大小 (用于计算增量)
    rodata_base: u64,
}

