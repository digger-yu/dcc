// src/codegen/asm_event.rs - Structured AsmEvent logging for -S output
// 
// This module provides a structured event log of code generation emit intent,
// enabling direct AT&T/GAS text formatting without relying on machine-code
// disassembly. Events track labels, directives, instructions, data sections,
// and relocations with full semantic information.

use crate::codegen::shared::elf::{ElfRela, R_X86_64_PC32, R_X86_64_PLT32, R_X86_64_64};


// (其余代码保持不变 - 简化处理移除了部分问题字段)

/// Assembly event types - the primary log structure emitted during codegen
#[derive(Clone, Debug)]
pub enum AsmEvent {
    /// Section directive: .section name,"flags"
    Section {
        name: String,
        flags: String,
    },
    /// Global symbol declaration: .globl symname
    Global {
        symbol: String,
    },
    /// Type attribute: .type symname,@function or @object
    Type {
        symbol: String,
        ty: AsmSymbolType,
    },
    /// Size attribute: .size symname,. -symname
    Size {
        symbol: String,
        offset_expr: Option<String>,
    },
    /// Label definition: .Llabel or function entry point
    Label {
        symbol: String,
    },
    /// Generic directive: raw assembler line (e.g., .cfi_startproc)
    Directive {
        text: String,
    },
    /// Instruction with structured operands
    Inst(AsmInst),
    /// Raw bytes for unknown/unsupported instructions
    Bytes {
        bytes: Vec<u8>,
        reason: ByteReason,
    },
    /// Data section content
    Data(AsmData),
}

#[derive(Clone, Debug)]
pub enum ByteReason {
    /// Inline asm not supported by mini-encoder
    InlineAsmUnsupported,
    /// GCC builtin / compiler-internal sequence
    BuiltinSequence,
    /// Unrecognized opcode pattern
    UnknownOpcode,
}

#[derive(Clone, Debug)]
pub enum AsmSymbolType {
    Function,
    Object,
    IndirectFunction, // @gnu_indirect_function for ifunc
}

impl AsmSymbolType {
    pub fn to_str(&self) -> &'static str {
        match self {
            AsmSymbolType::Function => "@function",
            AsmSymbolType::Object => "@object",
            AsmSymbolType::IndirectFunction => "@gnu_indirect_function",
        }
    }
}

/// Formatted assembly instruction
#[derive(Clone, Debug)]
pub struct AsmInst {
    pub offset: u64,           // Code offset for debug/assertion
    pub bytes_len: u8,         // Must match actual emitted bytes length
    pub mnemonic: AsmMnemonic,
    pub operands: Vec<AsmOperand>,
    pub prefixes: AsmPrefixes,
    pub comment: Option<String>,
}

impl AsmInst {
    pub fn new(offset: u64, bytes_len: u8, mnemonic: AsmMnemonic, operands: Vec<AsmOperand>) -> Self {
        Self {
            offset,
            bytes_len,
            mnemonic,
            operands,
            prefixes: AsmPrefixes::default(),
            comment: None,
        }
    }

    pub fn with_prefixes(mut self, prefixes: AsmPrefixes) -> Self {
        self.prefixes = prefixes;
        self
    }

    pub fn with_comment(mut self, comment: Option<String>) -> Self {
        self.comment = comment;
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct AsmPrefixes {
    pub lock: bool,
    pub rep: bool,
    pub repne: bool,
    pub data16: bool,
}

#[derive(Clone, Debug)]
pub enum AsmMnemonic {
    Mov, Add, Sub, And, Or, Xor, Cmp, Test,
    Lea, MovzxB, MovzxW, MovsxB, MovsxW, Movsxd,
    Push, Pop, Call, Ret, Jmp,
    Je, Jne, Jl, Jle, Jg, Jge, Jb, Jbe, Ja, Jae,
    Js, Jns, Jp, Jnp, Jo, Jno,
    CmovO, CmovNo, CmovB, CmovAe, CmovE, CmovNe, CmovBe, CmovA,
    CmovS, CmovNs, CmovP, CmovNp, CmovL, CmovGe, CmovLe, CmovG,
    SetO, SetNo, SetB, SetAe, SetE, SetNe, SetBe, SetA,
    SetS, SetNs, SetP, SetNp, SetL, SetGe, SetLe, SetG,
    Xchg, Xadd, Mfence, LockCmpxchg,
    Nop, Pause, Cltd, Cqo, Cwde, Cdqe,
    Movd, Movq, Movss, Movsd, Movaps, Movups,
    Cvtsi2ss, Cvtsi2sd, Cvttss2si, Cvttsd2si, Cvtss2sd, Cvtsd2ss,
    Pxor, Punpcklqdq, Punpckhqdq,
    // Add more as needed...
}

impl AsmMnemonic {
    pub fn to_str(&self) -> &'static str {
        match self {
            AsmMnemonic::Mov => "mov",
            AsmMnemonic::Add => "add",
            AsmMnemonic::Sub => "sub",
            AsmMnemonic::And => "and",
            AsmMnemonic::Or => "or",
            AsmMnemonic::Xor => "xor",
            AsmMnemonic::Cmp => "cmp",
            AsmMnemonic::Test => "test",
            AsmMnemonic::Lea => "lea",
            AsmMnemonic::MovzxB => "movzb",
            AsmMnemonic::MovzxW => "movzw",
            AsmMnemonic::MovsxB => "movsb",
            AsmMnemonic::MovsxW => "movsw",
            AsmMnemonic::Movsxd => "movsxd",
            AsmMnemonic::Push => "push",
            AsmMnemonic::Pop => "pop",
            AsmMnemonic::Call => "call",
            AsmMnemonic::Ret => "ret",
            AsmMnemonic::Jmp => "jmp",
            AsmMnemonic::Je => "je",
            AsmMnemonic::Jne => "jne",
            AsmMnemonic::Jl => "jl",
            AsmMnemonic::Jle => "jle",
            AsmMnemonic::Jg => "jg",
            AsmMnemonic::Jge => "jge",
            AsmMnemonic::Jb => "jb",
            AsmMnemonic::Jbe => "jbe",
            AsmMnemonic::Ja => "ja",
            AsmMnemonic::Jae => "jae",
            AsmMnemonic::Js => "js",
            AsmMnemonic::Jns => "jns",
            AsmMnemonic::Jp => "jp",
            AsmMnemonic::Jnp => "jnp",
            AsmMnemonic::Jo => "jo",
            AsmMnemonic::Jno => "jno",
            AsmMnemonic::CmovO => "cmovo",
            AsmMnemonic::CmovNo => "cmovno",
            AsmMnemonic::CmovB => "cmovb",
            AsmMnemonic::CmovAe => "cmovae",
            AsmMnemonic::CmovE => "cmove",
            AsmMnemonic::CmovNe => "cmovne",
            AsmMnemonic::CmovBe => "cmovbe",
            AsmMnemonic::CmovA => "cmova",
            AsmMnemonic::CmovS => "cmovs",
            AsmMnemonic::CmovNs => "cmovns",
            AsmMnemonic::CmovP => "cmovp",
            AsmMnemonic::CmovNp => "cmovnp",
            AsmMnemonic::CmovL => "cmovl",
            AsmMnemonic::CmovGe => "cmovge",
            AsmMnemonic::CmovLe => "cmovle",
            AsmMnemonic::CmovG => "cmovg",
            AsmMnemonic::SetO => "seto",
            AsmMnemonic::SetNo => "setno",
            AsmMnemonic::SetB => "setb",
            AsmMnemonic::SetAe => "setae",
            AsmMnemonic::SetE => "sete",
            AsmMnemonic::SetNe => "setne",
            AsmMnemonic::SetBe => "setbe",
            AsmMnemonic::SetA => "seta",
            AsmMnemonic::SetS => "sets",
            AsmMnemonic::SetNs => "setns",
            AsmMnemonic::SetP => "setp",
            AsmMnemonic::SetNp => "setnp",
            AsmMnemonic::SetL => "setl",
            AsmMnemonic::SetGe => "setge",
            AsmMnemonic::SetLe => "setle",
            AsmMnemonic::SetG => "setg",
            AsmMnemonic::Xchg => "xchg",
            AsmMnemonic::Xadd => "xadd",
            AsmMnemonic::Mfence => "mfence",
            AsmMnemonic::LockCmpxchg => "lock cmpxchg",
            AsmMnemonic::Nop => "nop",
            AsmMnemonic::Pause => "pause",
            AsmMnemonic::Cltd => "cltd",
            AsmMnemonic::Cqo => "cqo",
            AsmMnemonic::Cwde => "cwde",
            AsmMnemonic::Cdqe => "cdqe",
            AsmMnemonic::Movd => "movd",
            AsmMnemonic::Movq => "movq",
            AsmMnemonic::Movss => "movss",
            AsmMnemonic::Movsd => "movsd",
            AsmMnemonic::Movaps => "movaps",
            AsmMnemonic::Movups => "movups",
            AsmMnemonic::Cvtsi2ss => "cvtsi2ss",
            AsmMnemonic::Cvtsi2sd => "cvtsi2sd",
            AsmMnemonic::Cvttss2si => "cvtts2si",
            
            AsmMnemonic::Cvtss2sd => "cvtss2sd",
            AsmMnemonic::Cvtss2sd => "cvtss2sd",
            AsmMnemonic::Cvtsd2ss => "cvtsd2ss",
            AsmMnemonic::Cvttsd2si => "cvttsd2si",
            AsmMnemonic::Pxor => "pxor",
            AsmMnemonic::Punpcklqdq => "punpcklqdq",
            AsmMnemonic::Punpckhqdq => "punpckhqdq",
        }
    }
}

/// AT&T-style operand
#[derive(Clone, Debug)]
pub enum AsmOperand {
    Reg { class: RegClass, index: u8, width: OperandWidth },
    Imm { value: i64, width: OperandWidth },
    Mem(AsmMem),
    Symbol { name: String, reloc: Option<RelocKind> },
    Label { name: String },
    Zero, // $0
}

#[derive(Clone, Debug, Copy)]
pub enum RegClass {
    Gp,     // General purpose (rax, rbx, ...)
    Xmm,    // SSE/XMM
    Ymm,    // AVX/YMM
    Zmm,    // AVX-512/ZMM
    Mmx,    // MMX
    Segment, // ES, CS, SS, DS, FS, GS
    Control, // CR0-CR15
    Debug,   // DR0-DR7
}

#[derive(Clone, Debug, Copy)]
pub enum OperandWidth {
    Bit8,
    Bit16,
    Bit32,
    Bit64,
}

impl OperandWidth {
    pub fn suffix_char(&self) -> char {
        match *self {
            OperandWidth::Bit8 => 'b',
            OperandWidth::Bit16 => 'w',
            OperandWidth::Bit32 => 'l',
            OperandWidth::Bit64 => 'q',
        }
    }
}

/// Memory operand: disp(base,index,scale) or symbol@offset(RIP)
#[derive(Clone, Debug)]
pub struct AsmMem {
    pub symbol: Option<String>,
    pub base: Option<RegClass>,
    pub index: Option<RegClass>,
    pub scale: u8,
    pub displacement: i64,
    pub rip_relative: bool,
    pub width_hint: Option<OperandWidth>,
}

impl AsmMem {
    pub fn new() -> Self {
        Self {
            symbol: None,
            base: None,
            index: None,
            scale: 1,
            displacement: 0,
            rip_relative: false,
            width_hint: None,
        }
    }

    pub fn with_symbol(mut self, sym: String) -> Self {
        self.symbol = Some(sym);
        self
    }

    pub fn with_base(mut self, reg: RegClass) -> Self {
        self.base = Some(reg);
        self
    }

    pub fn with_index(mut self, reg: RegClass, scale: u8) -> Self {
        self.index = Some(reg);
        self.scale = scale;
        self
    }

    pub fn with_displacement(mut self, disp: i64) -> Self {
        self.displacement = disp;
        self
    }

    pub fn with_rip_relative(mut self) -> Self {
        self.rip_relative = true;
        self
    }
}

/// Relocation kinds for symbol references
#[derive(Clone, Debug)]
pub enum RelocKind {
    Pc32,      // PC-relative 32-bit
    Plt32,     // PLT 32-bit (for external calls)
    Abs64,     // Absolute 64-bit
    GotPcRel,  // GOT PC-relative
    Got64,     // GOT absolute 64-bit
}

impl RelocKind {
    pub fn from_elf_rela(rela: &ElfRela) -> Option<Self> {
        match rela.rel_type {
            R_X86_64_PC32 => Some(RelocKind::Pc32),
            R_X86_64_PLT32 => Some(RelocKind::Plt32),
            R_X86_64_64 => Some(RelocKind::Abs64),
            _ => None,
        }
    }
}

/// Data section representation
#[derive(Clone, Debug)]
pub enum AsmData {
    Byte(Vec<u8>),
    Word(i32),
    QWord(i64),
    Asciz(String),
    Align(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_asm_mnemonic_to_str() {
        assert_eq!(AsmMnemonic::Mov.to_str(), "mov");
        assert_eq!(AsmMnemonic::Jmp.to_str(), "jmp");
        assert_eq!(AsmMnemonic::Movd.to_str(), "movd");
    }

    #[test]
    fn test_operand_width_suffix() {
        assert_eq!(OperandWidth::Bit8.suffix_char(), 'b');
        assert_eq!(OperandWidth::Bit16.suffix_char(), 'w');
        assert_eq!(OperandWidth::Bit32.suffix_char(), 'l');
        assert_eq!(OperandWidth::Bit64.suffix_char(), 'q');
    }
}

/// AT&T/GAS text formatter for AsmEvent events
pub struct AsmFormatter {
    /// Track current position for label references
    pc: u64,
    /// Symbol name to address map (for labels)
    symbol_addrs: std::collections::HashMap<String, u64>,
}

impl AsmFormatter {
    pub fn new() -> Self {
        Self {
            pc: 0,
            symbol_addrs: std::collections::HashMap::new(),
        }
    }

    pub fn with_symbol_addr(mut self, symbol: String, addr: u64) -> Self {
        self.symbol_addrs.insert(symbol, addr);
        self
    }

    /// Format a single event to string
    pub fn format_event(&mut self, event: &AsmEvent) -> String {
        match event {
            AsmEvent::Section { name, flags } => {
                if name == ".text" {
                    format!("\t.text\n")
                } else {
                    format!("\t.section {name},\"{flags}\"\n")
                }
            }
            AsmEvent::Global { symbol } => {
                format!("\t.globl {symbol}\n")
            }
            AsmEvent::Type { symbol, ty } => {
                format!("\t.type {symbol}, {}\n", ty.to_str())
            }
            AsmEvent::Size { symbol, offset_expr } => {
                match offset_expr {
                    Some(expr) => format!("\t.size {symbol},{expr}\n"),
                    None => format!("\t.size {symbol},. -{symbol}\n"),
                }
            }
            AsmEvent::Label { symbol } => {
                format!("{symbol}:\n")
            }
            AsmEvent::Directive { text } => {
                format!("\t{text}\n")
            }
            AsmEvent::Inst(inst) => {
                format_instruction(inst, &mut self.pc, &self.symbol_addrs)
            }
            AsmEvent::Bytes { bytes, reason } => {
                let byte_str: Vec<String> = bytes.iter()
                    .map(|b| format!("0x{:02x}", b))
                    .collect();
                match reason {
                    ByteReason::InlineAsmUnsupported => {
                        format!("\t# INLINE_ASM_UNSUPPORTED {}\n", byte_str.join(", "))
                    }
                    ByteReason::BuiltinSequence => {
                        format!("\t# BUILTIN_SEQUENCE {}\n", byte_str.join(", "))
                    }
                    ByteReason::UnknownOpcode => {
                        format!("\t# UNKNOWN_INSTRUCTION {}\n", byte_str.join(", "))
                    }
                }
            }
            AsmEvent::Data(data) => {
                format_data(data)
            }
        }
    }

    /// Format multiple events to full text output
    pub fn format_events(&mut self, events: &[&AsmEvent]) -> String {
        events.iter().map(|e| self.format_event(e)).collect::<Vec<_>>().join("")
    }
}

impl Default for AsmFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Format an instruction event to AT&T syntax
fn format_instruction(inst: &AsmInst, pc: &mut u64, symbol_addrs: &std::collections::HashMap<String, u64>) -> String {
    // Update PC based on instruction length
    *pc += inst.bytes_len as u64;

    // Build prefix string
    let mut parts = Vec::new();
    if inst.prefixes.lock {
        parts.push("lock");
    }
    if inst.prefixes.repne {
        parts.push("repne");
    } else if inst.prefixes.rep {
        parts.push("rep");
    }
    if inst.prefixes.data16 {
        parts.push("data16");
    }
    let prefix_str = parts.join(" ");
    let prefix_space = if prefix_str.is_empty() { "" } else { " " };

    // Format mnemonic and operands
    let mnemonic = inst.mnemonic.to_str();
    let ops_str: Vec<String> = inst.operands.iter()
        .map(|op| format_operand(op, *pc, symbol_addrs))
        .collect();
    let ops_joined = ops_str.join(", ");

    // Assemble final line
    let line = if prefix_str.is_empty() {
        format!("\t{}{}", mnemonic, if ops_joined.is_empty() { String::new() } else { format!(" {}", ops_joined) })
    } else {
        format!("\t{}{} {}{}", prefix_str, prefix_space, mnemonic, if ops_joined.is_empty() { String::new() } else { format!(" {}", ops_joined) })
    };

    // Add comment if present
    match &inst.comment {
        Some(comment) => format!("{}\t# {}", line, comment),
        None => line,
    }
}

/// Format a single operand
fn format_operand(op: &AsmOperand, _pc: u64, symbol_addrs: &std::collections::HashMap<String, u64>) -> String {
    match op {
        AsmOperand::Reg { class: c, index, width } => {
            format_register(*c, *index, *width)
        }
        AsmOperand::Imm { value, width: _ } => {
            let imm_str = if *value >= 0 {
                format!("${}", value)
            } else {
                format!("{}", value)
            };
            // For AT&T, immediate doesn't have suffix in display
            imm_str
        }
        AsmOperand::Mem(mem) => {
            format_memory(mem, symbol_addrs)
        }
        AsmOperand::Symbol { name, reloc: _reloc } => {
            // Symbol-only reference (like call foo)
            name.clone()
        }
        AsmOperand::Label { name } => {
            format!(".L{}", name)
        }
        AsmOperand::Zero => "$0".to_string(),
    }
}

/// Format register operand with proper AT&T naming
fn format_register(class: RegClass, index: u8, width: OperandWidth) -> String {
    let prefix = match class {
        RegClass::Gp => {
            match width {
                OperandWidth::Bit8 => get_gp_name_8bit(index).to_string(),
                OperandWidth::Bit16 => get_gp_name_16bit(index).to_string(),
                OperandWidth::Bit32 => get_gp_name_32bit(index).to_string(),
                OperandWidth::Bit64 => format!("%r{}", get_gp_name_64bit(index)),
            }
        }
        RegClass::Xmm => format!("%xmm{}", index),
        RegClass::Ymm => format!("%ymm{}", index),
        RegClass::Zmm => format!("%zmm{}", index),
        RegClass::Mmx => format!("%mm{}", index),
        RegClass::Segment => get_seg_name(index),
        RegClass::Control => format!("cr{}", index),
        RegClass::Debug => format!("dr{}", index),
    };
    format!("{}", prefix)
}

fn get_gp_name_8bit(idx: u8) -> &'static str {
    const NAMES: [&str; 16] = [
        "%al", "%cl", "%dl", "%bl", "%spl", "%bpl", "%sil", "%dil",
        "%r8b", "%r9b", "%r10b", "%r11b", "%r12b", "%r13b", "%r14b", "%r15b"
    ];
    NAMES[idx as usize % 16]
}

fn get_gp_name_16bit(idx: u8) -> &'static str {
    const NAMES: [&str; 16] = [
        "%ax", "%cx", "%dx", "%bx", "%sp", "%bp", "%si", "%di",
        "%r8w", "%r9w", "%r10w", "%r11w", "%r12w", "%r13w", "%r14w", "%r15w"
    ];
    NAMES[idx as usize % 16]
}

fn get_gp_name_32bit(idx: u8) -> &'static str {
    const NAMES: [&str; 16] = [
        "%eax", "%ecx", "%edx", "%ebx", "%esp", "%ebp", "%esi", "%edi",
        "%r8d", "%r9d", "%r10d", "%r11d", "%r12d", "%r13d", "%r14d", "%r15d"
    ];
    NAMES[idx as usize % 16]
}

fn get_gp_name_64bit(idx: u8) -> &'static str {
    const NAMES: [&str; 16] = [
        "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi",
        "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"
    ];
    NAMES[idx as usize % 16]
}

fn get_seg_name(idx: u8) -> String {
    match idx {
        0 => "%es".to_string(),
        1 => "%cs".to_string(),
        2 => "%ss".to_string(),
        3 => "%ds".to_string(),
        4 => "%fs".to_string(),
        5 => "%gs".to_string(),
        _ => format!("%seg{}", idx),
    }
}

/// Format memory operand in AT&T syntax
fn format_memory(mem: &AsmMem, _symbol_addrs: &std::collections::HashMap<String, u64>) -> String {
    let mut parts = Vec::new();

    // Displacement or symbol
    if let Some(sym) = &mem.symbol {
        if mem.displacement != 0 {
            if mem.displacement > 0 {
                parts.push(format!("{}+{}", sym, mem.displacement));
            } else {
                parts.push(format!("{}{}", sym, mem.displacement));
            }
        } else {
            parts.push(sym.clone());
        }
    } else if mem.displacement != 0 {
        parts.push(format!("{}", mem.displacement));
    }

    // Base register
    if let Some(base) = &mem.base {
    let base_reg = match base {
            RegClass::Gp => format!("%r{}", get_gp_name_64bit(13)), // Use r13 as r13 convention
            _ => format!("%invalid"),
        };
        parts.push(base_reg);
    } else if mem.rip_relative && mem.symbol.is_none() {
        parts.push("%rip".to_string());
    }

    // Index*scale
if let Some(_index) = mem.index {
        if mem.scale != 1 {
            parts.push(format!("%r{},{}", get_gp_name_64bit(13), mem.scale));
        } else {
            parts.push(format!("%r{}", get_gp_name_64bit(13)));
        }
    }

    // Build memory expression
    if parts.is_empty() {
        "0".to_string()
    } else if parts.len() == 1 && !parts[0].contains('(') {
        if mem.base.is_some() || mem.rip_relative {
            format!("({})", parts[0])
        } else {
            parts[0].clone()
        }
    } else {
        // Handle complex addressing
        let disp_part = if parts[0].contains('+') || parts[0].contains('-') {
            format!("({},", parts[0])
        } else if !parts[0].starts_with('%') {
            format!("({},", parts[0])
        } else {
            "(".to_string()
        };
        
        let remaining: Vec<String> = parts[1..].iter().map(|p| p.clone()).collect();
        format!("{}{})", disp_part, remaining.join(","))
    }
}

/// Format data section content
fn format_data(data: &AsmData) -> String {
    match data {
        AsmData::Byte(bytes) => {
            let hex: Vec<String> = bytes.iter().map(|b| format!("0x{:02x}", b)).collect();
            format!("\t.byte {}", hex.join(", "))
        }
        AsmData::Word(val) => {
            format!("\t.long {}", val)
        }
        AsmData::QWord(val) => {
            format!("\t.quad {}", val)
        }
        AsmData::Asciz(string) => {
            // Escape quotes and backslashes
            let escaped = string.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\t.string \"{}\"", escaped)
        }
        AsmData::Align(n) => {
            format!("\t.align {}", n)
        }
    }
}
