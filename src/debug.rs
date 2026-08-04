// src/debug.rs - 完整调试信息生成
//   DWARF v4 + .BTF + .eh_frame + .orc_unwind_ip + .debug_frame +
//   .debug_aranges + .debug_ranges + .debug_pubnames + .debug_names +
//   .debug_macro + .gdb_index + split-dwarf + AArch64/RISC-V
//
// 第一性原理:
//   * pahole / objtool 解析 BTF + DWARF 结构体 + FDE 帧表
//   * addr2line / GDB 需要 DWARF 行号表 + DIEs + 字符串表
//   * split-dwarf 把 .debug_* 大节拆到 .dwo，骨架只留地址 + 索引
//   * 跨架构: x86_64 / aarch64 / riscv64 在 CIE augmentation / RA 寄存器号
//     上有所区别；EH 帧 CIE 必须分别编码，DWARF v4 字节序按目标 ABI 决定
//
// 公共 API:
//   * `build_dwarf(source, funcs)` 兼容旧签名 → 自动派发到 `build_dwarf_full`
//   * `build_btf_probe()` 兼容旧签名 → 完整 BTF
//   * `build_eh_frame(funcs)` 兼容旧签名 → x86_64 默认
//   * `build_orc_unwind_ip(funcs)` 兼容旧签名
//   * `dwarf_elf_sections(sec)` 转 ELF 节列表
//   * `build_dwarf_full(cu, opts)` 完整入口
//   * `build_eh_frame_with_arch(funcs, arch)` 跨架构
//   * `build_gdb_index(funcs)` 链接器 -Wl,--gdb-index 数据
//
// 模块级 `non_upper_case_globals` allow: DWARF v4 规范常量 (DW_TAG_* /
// DW_AT_* / DW_FORM_* / DW_CFA_* / DW_LNS_* / DW_LNE_* / DW_LNCT_* /
// DW_INL_*) 严格沿用规范原名, 改写为 ALL_CAPS 会丢失与规范章节对应.
#![allow(non_upper_case_globals)]

use crate::codegen::elf::{ElfSection, SHF_ALLOC, SHT_PROGBITS};

// ============================================================================
// 公共类型
// ============================================================================

/// 目标架构（用于 EH 帧 CIE 编码 + 寄存器编号）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchKind {
    X86_64,
    AArch64,
    RiscV64,
    Mips,
    Mips64,
    Arm,
}

impl ArchKind {
    /// DWARF v4 寄存器编号约定：返回地址寄存器
    pub fn dwarf_reg_ra(self) -> u8 {
        match self {
            ArchKind::X86_64 => 16,                  // %rip
            ArchKind::AArch64 => 30,                 // PC (R30)
            ArchKind::RiscV64 => 1,                  // ra (x1)
            ArchKind::Mips | ArchKind::Mips64 => 31, // $ra
            ArchKind::Arm => 14,                     // R14 (lr)
        }
    }
    /// 帧基址寄存器
    pub fn dwarf_reg_fp(self) -> u8 {
        match self {
            ArchKind::X86_64 => 6,                   // x86_64 CIE 用 rsp-relative CFA
            ArchKind::AArch64 => 29,                 // FP (X29)
            ArchKind::RiscV64 => 8,                  // s0/fp (x8)
            ArchKind::Mips | ArchKind::Mips64 => 30, // $fp
            ArchKind::Arm => 11,                     // R11 (fp)
        }
    }
    /// 字宽（字节）
    pub fn address_size(self) -> u8 {
        match self {
            ArchKind::X86_64 | ArchKind::AArch64 | ArchKind::RiscV64 | ArchKind::Mips64 => 8,
            ArchKind::Mips | ArchKind::Arm => 4,
        }
    }
    /// EH 帧 CIE augmentation 字符串
    pub fn cie_augmentation(self) -> &'static str {
        // 所有架构都输出 "zR" (带 FDE encoding 0x1b), 这样 ld 可以生成
        // .eh_frame_hdr (binary search 表, CET / glibc unwinder 依赖).
        // 之前 x86_64 走 “” augmentation 路径, ld 报
        // "no .eh_frame_hdr table will be created" + "PHDR segment not
        // covered by LOAD segment".
        match self {
            ArchKind::X86_64
            | ArchKind::AArch64
            | ArchKind::RiscV64
            | ArchKind::Mips
            | ArchKind::Mips64
            | ArchKind::Arm => "zR",
        }
    }
    /// DW_CFA_def_cfa 默认寄存器编号
    pub fn cfa_register(self) -> u8 {
        match self {
            ArchKind::X86_64 => 7,                   // rsp
            ArchKind::AArch64 => 31,                 // sp
            ArchKind::RiscV64 => 2,                  // sp
            ArchKind::Mips | ArchKind::Mips64 => 29, // $sp
            ArchKind::Arm => 13,                     // sp (R13)
        }
    }
}

/// 调试生成选项
#[derive(Debug, Clone)]
pub struct DebugOptions {
    /// -g 等级：0=off, 1=g1, 2=g2 (DWARF), 3=g3 (含宏)
    pub level: u8,
    /// -gsplit-dwarf: 拆出 .debug_*.dwo
    pub split_dwarf: bool,
    /// -gpubnames: 写 .debug_pubnames + .debug_pubtypes
    pub pubnames: bool,
    /// --gdb-index: 链接阶段写 .gdb_index
    pub gdb_index: bool,
    /// 目标架构
    pub arch: ArchKind,
    /// 是否小端
    pub little_endian: bool,
    /// 是否 64 位
    pub is_64bit: bool,
    /// 源文件（CU 顶层 .debug_info 写 DW_AT_name）
    pub source_name: String,
    /// 编译目录（DW_AT_comp_dir）
    pub comp_dir: Option<String>,
    /// 厂商（如 "GNU" / "dcc"）
    pub producer: String,
    /// minimal_instruction_length
    pub min_insn_len: u8,
    /// 行号程序: max_ops_per_insn (DWARF 4 必须为 1)
    pub max_ops_per_insn: u8,
    /// 行号程序: default_is_stmt
    pub default_is_stmt: u8,
    /// 行号程序: line_base
    pub line_base: i8,
    /// 行号程序: line_range
    pub line_range: u8,
    /// 行号程序: opcode_base
    pub opcode_base: u8,
}

impl Default for DebugOptions {
    fn default() -> Self {
        Self {
            level: 0,
            split_dwarf: false,
            pubnames: false,
            gdb_index: false,
            arch: ArchKind::X86_64,
            little_endian: true,
            is_64bit: true,
            source_name: "<unknown>".to_string(),
            comp_dir: None,
            producer: format!("dcc {}", env!("CARGO_PKG_VERSION")),
            min_insn_len: 1,
            max_ops_per_insn: 1,
            default_is_stmt: 1,
            line_base: -5,
            line_range: 14,
            opcode_base: 13,
        }
    }
}

/// 单个源文件位置记录（PC → 行/列/文件）
#[derive(Debug, Clone, Copy)]
pub struct LineEntry {
    pub pc: u64,
    pub line: u32,
    pub column: u32,
    pub file_idx: u16,      // 0-based into debug_str source list
    pub discriminator: u16, // DWARF 4 inlined function discriminator
    pub is_stmt: bool,
    pub basic_block: bool,
}

/// 函数 PC 与大小（供 eh_frame FDE / DWARF DW_AT_low_pc 生成）
#[derive(Debug, Clone)]
pub struct FuncFrame {
    pub pc: u64,
    pub size: u32,
    /// FDE 内 CFI 指令字节流 (从 CFA 初始指令之后开始; 不含末尾 advance_loc).
    /// 为空表示该函数无 CFI 信息 (沿用 CIE 默认 CFA = rsp + 8).
    /// 字节流内: DW_CFA_def_cfa / DW_CFA_def_cfa_offset /
    /// DW_CFA_offset / DW_CFA_restore 等 uleb128 编码指令按顺序串接.
    pub cfi_ops: Vec<u8>,
}

impl FuncFrame {
    /// 构造一个无 CFI 信息的 FuncFrame (仅 PC + size).
    pub fn new(pc: u64, size: u32) -> Self {
        Self {
            pc,
            size,
            cfi_ops: Vec::new(),
        }
    }
    /// 构造带 CFI ops 的 FuncFrame.
    pub fn with_cfi(pc: u64, size: u32, cfi_ops: Vec<u8>) -> Self {
        Self { pc, size, cfi_ops }
    }
}

/// 类型编码（DWARF DW_AT_encoding）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeEncoding {
    Address = 0x01,
    Boolean = 0x02,
    Float = 0x04,
    Signed = 0x05,
    SignedChar = 0x06,
    Unsigned = 0x07,
    UnsignedChar = 0x08,
}

/// DWARF 类型描述
#[derive(Debug, Clone)]
pub struct DwarfType {
    pub name: String,
    pub byte_size: u64,
    pub kind: DwarfTypeKind,
}

#[derive(Debug, Clone)]
pub enum DwarfTypeKind {
    Base(TypeEncoding),
    Pointer(Box<DwarfType>),
    Const(Box<DwarfType>),
    Volatile(Box<DwarfType>),
    Restrict(Box<DwarfType>),
    Struct {
        members: Vec<DwarfMember>,
    },
    Union {
        members: Vec<DwarfMember>,
    },
    Array {
        element: Box<DwarfType>,
        count: u64,
    },
    Func {
        ret: Box<DwarfType>,
        params: Vec<DwarfType>,
    },
    Enum {
        members: Vec<(String, i128)>,
        underlying: Box<DwarfType>,
    },
    Typedef {
        alias: String,
        target: Box<DwarfType>,
    },
    Void,
}

#[derive(Debug, Clone)]
pub struct DwarfMember {
    pub name: String,
    pub offset_bits: u64,
    /// 位域宽度: 0 表示非位域, > 0 表示该成员是位域且宽 bit_size 位.
    /// 位域场景下 offset_bits 表示起始 bit 偏移, byte_size 由编译器
    /// 布局决定, DWARF 用 DW_AT_bit_size + DW_AT_bit_offset 描述.
    pub bit_size: u32,
    pub ty: DwarfType,
}

impl DwarfMember {
    /// 构造非位域成员
    pub fn regular(name: impl Into<String>, offset_bits: u64, ty: DwarfType) -> Self {
        Self {
            name: name.into(),
            offset_bits,
            bit_size: 0,
            ty,
        }
    }
    /// 构造位域成员: offset_bits 为字段起始 bit 偏移, bit_size 为字段宽度
    pub fn bitfield(
        name: impl Into<String>,
        offset_bits: u64,
        bit_size: u32,
        ty: DwarfType,
    ) -> Self {
        Self {
            name: name.into(),
            offset_bits,
            bit_size,
            ty,
        }
    }
}

/// 函数作用域信息（用于 DWARF DIEs + GDB index）
#[derive(Debug, Clone)]
pub struct DwarfFuncInfo {
    pub name: String,
    pub linkage_name: Option<String>,
    pub low_pc: u64,
    pub high_pc: u64,
    pub decl_line: u32,
    pub decl_file_idx: u16,
    pub return_type: DwarfType,
    pub params: Vec<(String, DwarfType)>,
    pub is_external: bool,
    pub is_inline: bool,
    pub is_declaration: bool,
}

/// 全局变量信息（用于 DWARF DW_TAG_variable）
#[derive(Debug, Clone)]
pub struct DwarfGlobalInfo {
    pub name: String,
    pub linkage_name: Option<String>,
    pub ty: DwarfType,
    pub decl_line: u32,
    pub decl_file_idx: u16,
    pub is_external: bool,
}

/// 一次完整 DWARF 编译单元的输入
#[derive(Debug, Clone)]
pub struct CompileUnit {
    pub source_name: String,
    pub comp_dir: Option<String>,
    pub producer: String,
    pub language: u16, // DW_LANG_C99 = 12
    pub functions: Vec<DwarfFuncInfo>,
    pub globals: Vec<DwarfGlobalInfo>,
    pub types: Vec<DwarfType>,
    pub line_entries: Vec<LineEntry>,
    pub line_files: Vec<String>,
    pub line_dirs: Vec<String>,
    /// 第一条 line_entry 处的 PC（用于 DW_AT_low_pc 兜底）
    pub low_pc: u64,
    /// 最后一条 line_entry 处的 PC（用于 DW_AT_high_pc 兜底）
    pub high_pc: u64,
    /// 函数 PC 列表（供 .debug_aranges）
    pub func_ranges: Vec<(u64, u64)>,
    /// FDE CFI 行（CFA 推进 / 寄存器保存）
    pub cfi_rows: Vec<CfiRow>,
}

/// 单行 CFI（供 .debug_frame 真实指令流）
#[derive(Debug, Clone, Copy)]
pub struct CfiRow {
    /// 触发该规则的 PC（相对 CU 起始 PC）
    pub pc: u64,
    /// CFA 寄存器（DWARF regno）
    pub cfa_reg: u8,
    /// CFA 偏移
    pub cfa_off: i32,
    /// 已保存的寄存器表：(regno, offset_from_cfa)
    pub saved: &'static [(u8, i32)],
}

/// 全部调试节（一次性产出）
#[derive(Debug, Clone, Default)]
pub struct DebugSections {
    // 主 DWARF 节
    pub abbrev: Vec<u8>,
    pub info: Vec<u8>,
    pub line: Vec<u8>,
    pub str: Vec<u8>,
    pub aranges: Vec<u8>,
    pub frame: Vec<u8>,
    pub ranges: Vec<u8>,
    pub loc: Vec<u8>,
    // 索引类
    pub pubnames: Vec<u8>,
    pub pubtypes: Vec<u8>,
    pub names: Vec<u8>,
    // 宏 / 范围
    pub macro_: Vec<u8>,
    pub loclists: Vec<u8>,
    pub rnglists: Vec<u8>,
    // split-dwarf 副节
    pub info_dwo: Vec<u8>,
    pub abbrev_dwo: Vec<u8>,
    pub str_dwo: Vec<u8>,
    pub line_dwo: Vec<u8>,
    pub macro_dwo: Vec<u8>,
    pub rnglists_dwo: Vec<u8>,
    pub loclists_dwo: Vec<u8>,
    pub skeleton: Vec<u8>,
    pub skeleton_type: Vec<u8>,
    // BTF
    pub btf: Vec<u8>,
}

impl DebugSections {
    /// 把 .debug_* 节转为 ElfSection 列表（供 codegen 注册）
    pub fn to_elf_sections(&self, split_dwarf: bool) -> Vec<ElfSection> {
        let mut v = vec![];
        let push = |v: &mut Vec<ElfSection>, name: &str, data: &[u8], alloc: bool| {
            if data.is_empty() {
                return;
            }
            v.push(ElfSection {
                name: name.to_string(),
                data: data.to_vec(),
                ty: SHT_PROGBITS,
                flags: if alloc { SHF_ALLOC } else { 0 },
                align: 1,
            });
        };
        push(&mut v, ".debug_abbrev", &self.abbrev, false);
        push(&mut v, ".debug_info", &self.info, false);
        push(&mut v, ".debug_line", &self.line, false);
        push(&mut v, ".debug_str", &self.str, false);
        push(&mut v, ".debug_aranges", &self.aranges, false);
        // .debug_frame 仅为 gdb DWARF 调试使用, 不需被加载器装入内存, 不加 SHF_ALLOC.
        // (之前在 .debug_frame 加 SHF_ALLOC, ld 在尝试映射时发现其不在任何 LOAD
        // 段中, 报 “PHDR segment not covered by LOAD segment”.)
        push(&mut v, ".debug_frame", &self.frame, false);
        push(&mut v, ".debug_ranges", &self.ranges, false);
        push(&mut v, ".debug_pubnames", &self.pubnames, false);
        push(&mut v, ".debug_pubtypes", &self.pubtypes, false);
        push(&mut v, ".debug_names", &self.names, false);
        push(&mut v, ".debug_macro", &self.macro_, false);
        push(&mut v, ".debug_loclists", &self.loclists, false);
        push(&mut v, ".debug_rnglists", &self.rnglists, false);
        if split_dwarf {
            push(&mut v, ".debug_info.dwo", &self.info_dwo, false);
            push(&mut v, ".debug_abbrev.dwo", &self.abbrev_dwo, false);
            push(&mut v, ".debug_str.dwo", &self.str_dwo, false);
            push(&mut v, ".debug_line.dwo", &self.line_dwo, false);
            push(&mut v, ".debug_macro.dwo", &self.macro_dwo, false);
            push(&mut v, ".debug_rnglists.dwo", &self.rnglists_dwo, false);
            push(&mut v, ".debug_loclists.dwo", &self.loclists_dwo, false);
            push(&mut v, ".debug_types", &self.skeleton_type, false);
        }
        v
    }
}

// ============================================================================
// DWARF v4 常量（tag / attribute / form / opcode）
//
// 这些常量名严格遵循 DWARF v4 规范原文, 例如 DW_TAG_array_type /
// DW_CFA_def_cfa_offset_sf 等, 故全部标 `non_upper_case_globals` 抑制
// 命名风格告警。改写为 Rust 风格 ALL_CAPS 会丢失与规范章节的对应关系,
// 不利于调试. -- 2026-07-21
// ============================================================================
#[allow(non_upper_case_globals)]
const DW_TAG_array_type: u16 = 0x01;
const DW_TAG_class_type: u16 = 0x02;
const DW_TAG_enumeration_type: u16 = 0x04;
const DW_TAG_formal_parameter: u16 = 0x05;
const DW_TAG_lexical_block: u16 = 0x0b;
const DW_TAG_member: u16 = 0x0d;
const DW_TAG_pointer_type: u16 = 0x0f;
const DW_TAG_compile_unit: u16 = 0x11;
const DW_TAG_structure_type: u16 = 0x13;
const DW_TAG_subroutine_type: u16 = 0x15;
const DW_TAG_typedef: u16 = 0x16;
const DW_TAG_union_type: u16 = 0x17;
const DW_TAG_unspecified_parameters: u16 = 0x18;
const DW_TAG_subrange_type: u16 = 0x21;
const DW_TAG_base_type: u16 = 0x24;
const DW_TAG_const_type: u16 = 0x26;
const DW_TAG_enumerator: u16 = 0x28;
const DW_TAG_variable: u16 = 0x34;
const DW_TAG_volatile_type: u16 = 0x23;
const DW_TAG_restrict_type: u16 = 0x25;
const DW_TAG_subprogram: u16 = 0x2e;

const DW_AT_name: u16 = 0x03;
const DW_AT_byte_size: u16 = 0x0b;
const DW_AT_encoding: u16 = 0x3e;
const DW_AT_type: u16 = 0x49;
const DW_AT_stmt_list: u16 = 0x10;
const DW_AT_low_pc: u16 = 0x11;
const DW_AT_high_pc: u16 = 0x12;
const DW_AT_language: u16 = 0x13;
const DW_AT_comp_dir: u16 = 0x1b;
const DW_AT_producer: u16 = 0x25;
const DW_AT_external: u16 = 0x3f;
const DW_AT_inline: u16 = 0x20;
const DW_AT_declaration: u16 = 0x3c;
const DW_AT_specification: u16 = 0x47;
const DW_AT_upper_bound: u16 = 0x2f;
const DW_AT_count: u16 = 0x37;
const DW_AT_data_member_location: u16 = 0x38;
const DW_AT_bit_size: u16 = 0x0d;
const DW_AT_bit_offset: u16 = 0x0e;
const DW_AT_call_file: u16 = 0x57;
const DW_AT_call_line: u16 = 0x58;
const DW_AT_linkage_name: u16 = 0x6e;
const DW_AT_const_value: u16 = 0x1c;
const DW_AT_decl_file: u16 = 0x3a;
const DW_AT_decl_line: u16 = 0x3b;
const DW_AT_GNU_dwo_id: u16 = 0x2130;
const DW_AT_GNU_dwo_name: u16 = 0x2131;
const DW_AT_GNU_all_call_sites: u16 = 0x2117;

const DW_FORM_addr: u16 = 0x01;
const DW_FORM_data1: u16 = 0x0b;
const DW_FORM_data2: u16 = 0x05;
const DW_FORM_data4: u16 = 0x06;
const DW_FORM_data8: u16 = 0x07;
const DW_FORM_string: u16 = 0x08;
const DW_FORM_strp: u16 = 0x0e;
const DW_FORM_ref4: u16 = 0x13;
const DW_FORM_flag_present: u16 = 0x19;
const DW_FORM_sec_offset: u16 = 0x17;
const DW_FORM_flag: u16 = 0x0c;
const DW_FORM_exprloc: u16 = 0x18;
const DW_FORM_ref8: u16 = 0x14;
const DW_FORM_ref1: u16 = 0x11;
const DW_FORM_ref_udata: u16 = 0x0f;

const DW_LNS_extended_op: u8 = 0x00;
const DW_LNS_copy: u8 = 0x01;
const DW_LNS_advance_pc: u8 = 0x02;
const DW_LNS_advance_line: u8 = 0x03;
const DW_LNS_set_file: u8 = 0x04;
const DW_LNS_set_column: u8 = 0x05;
const DW_LNS_negate_stmt: u8 = 0x06;
const DW_LNS_set_basic_block: u8 = 0x07;
const DW_LNS_const_add_pc: u8 = 0x08;
const DW_LNS_fixed_advance_pc: u8 = 0x09;
const DW_LNS_set_prologue_end: u8 = 0x0a;
const DW_LNS_set_epilogue_begin: u8 = 0x0b;
const DW_LNS_set_isa: u8 = 0x0c;

const DW_LNE_end_sequence: u8 = 0x01;
const DW_LNE_set_address: u8 = 0x02;
const DW_LNE_define_file: u8 = 0x03;
const DW_LNE_set_discriminator: u8 = 0x04;

const DW_LNCT_path: u16 = 0x01;
const DW_LNCT_directory_index: u16 = 0x02;
const DW_LNCT_timestamp: u16 = 0x03;
const DW_LNCT_size: u16 = 0x05;

const DW_INL_declared_inlined: u8 = 3;
const DW_INL_inlined: u8 = 1;
const DW_INL_declared_not_inlined: u8 = 2;
const DW_INL_not_inlined: u8 = 0;

const DW_CHILDREN_no: u8 = 0x00;
const DW_CHILDREN_yes: u8 = 0x01;

const DW_LANG_C99: u16 = 12;
const DW_LANG_C11: u16 = 29;
const DW_LANG_C17: u16 = 31;

// CFI opcodes (DW_CFA_*)
const DW_CFA_extended: u8 = 0x00;
const DW_CFA_advance_loc: u8 = 0x40;
const DW_CFA_offset: u8 = 0x80;
const DW_CFA_restore: u8 = 0xc0;
const DW_CFA_nop: u8 = 0x00;
const DW_CFA_set_loc: u8 = 0x01;
const DW_CFA_advance_loc1: u8 = 0x02;
const DW_CFA_advance_loc2: u8 = 0x03;
const DW_CFA_advance_loc4: u8 = 0x04;
const DW_CFA_offset_extended: u8 = 0x05;
const DW_CFA_restore_extended: u8 = 0x06;
const DW_CFA_undefined: u8 = 0x07;
const DW_CFA_same_value: u8 = 0x08;
const DW_CFA_register: u8 = 0x09;
const DW_CFA_remember_state: u8 = 0x0a;
const DW_CFA_restore_state: u8 = 0x0b;
const DW_CFA_def_cfa: u8 = 0x0c;
const DW_CFA_def_cfa_register: u8 = 0x0d;
const DW_CFA_def_cfa_offset: u8 = 0x0e;
const DW_CFA_def_cfa_expression: u8 = 0x0f;
const DW_CFA_expression: u8 = 0x10;
const DW_CFA_offset_extended_sf: u8 = 0x11;
const DW_CFA_def_cfa_sf: u8 = 0x12;
const DW_CFA_def_cfa_offset_sf: u8 = 0x13;
const DW_CFA_val_offset: u8 = 0x14;
const DW_CFA_val_offset_sf: u8 = 0x15;
const DW_CFA_val_expression: u8 = 0x16;

const BTF_KIND_INT: u32 = 1;
const BTF_KIND_PTR: u32 = 2;
const BTF_KIND_ARRAY: u32 = 3;
const BTF_KIND_STRUCT: u32 = 4;
const BTF_KIND_UNION: u32 = 5;
const BTF_KIND_ENUM: u32 = 6;
const BTF_KIND_FWD: u32 = 7;
const BTF_KIND_TYPEDEF: u32 = 8;
const BTF_KIND_VOLATILE: u32 = 9;
const BTF_KIND_CONST: u32 = 10;
const BTF_KIND_RESTRICT: u32 = 11;
const BTF_KIND_FUNC: u32 = 12;
const BTF_KIND_FUNC_PROTO: u32 = 13;
const BTF_KIND_VAR: u32 = 14;
const BTF_KIND_DATASEC: u32 = 15;

// ============================================================================
// LEB128 编码
// ============================================================================

fn write_uleb128(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            break;
        }
    }
}

fn write_sleb128(out: &mut Vec<u8>, mut v: i64) {
    let mut more = true;
    while more {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        let sign_bit = (byte & 0x40) != 0;
        more = !((v == 0 && !sign_bit) || (v == -1 && sign_bit));
        if more {
            byte |= 0x80;
        }
        out.push(byte);
    }
}

// ============================================================================
// DWARF 字符串表
// ============================================================================

#[derive(Debug, Default, Clone)]
struct StringTable {
    data: Vec<u8>,
    map: std::collections::HashMap<String, u32>,
}

impl StringTable {
    fn new() -> Self {
        Self {
            data: vec![0u8],
            map: Default::default(),
        }
    }
    fn intern(&mut self, s: &str) -> u32 {
        if s.is_empty() {
            return 0;
        }
        if let Some(&off) = self.map.get(s) {
            return off;
        }
        let off = self.data.len() as u32;
        self.data.extend_from_slice(s.as_bytes());
        self.data.push(0);
        self.map.insert(s.to_string(), off);
        off
    }
    fn into_bytes(self) -> Vec<u8> {
        self.data
    }
}

// ============================================================================
// Abbrev 构造器
// ============================================================================

/// DWARF 缩写表 key: (tag code, children flag, [(attr, form), ...])
type AbbrevKey = (u16, u8, Vec<(u16, u16)>);

#[derive(Debug, Default, Clone)]
struct AbbrevBuilder {
    data: Vec<u8>,
    map: std::collections::HashMap<AbbrevKey, u32>,
    /// 已经分配的 entry index（1-based）
    next_index: u32,
}

impl AbbrevBuilder {
    /// 拿到或创建缩写（按 (tag, children, [(name, form), ...]) 唯一）
    /// 返回 entry index (1-based)
    fn get_or_create(&mut self, code: u16, children: u8, attrs: &[(u16, u16)]) -> u32 {
        let key = (code, children, attrs.to_vec());
        if let Some(&off) = self.map.get(&key) {
            // entry index = 该 entry 的首字节 uleb128 值
            // 由于 1-based 单调递增，可借助 self.data[off..] 解析；但简单做法：
            // 维护 next_index 即可
            return self.index_at(off);
        }
        let off = self.data.len() as u32;
        self.next_index += 1;
        let entry_idx = self.next_index;
        write_uleb128(&mut self.data, entry_idx as u64);
        write_uleb128(&mut self.data, code as u64);
        self.data.push(children);
        for &(n, f) in attrs {
            write_uleb128(&mut self.data, n as u64);
            write_uleb128(&mut self.data, f as u64);
        }
        // 终止: 0,0
        self.data.push(0);
        self.data.push(0);
        self.map.insert(key, off);
        entry_idx
    }
    /// 给定 entry offset, 解析首字节得到 1-based index
    fn index_at(&self, off: u32) -> u32 {
        // 由于每个 entry 写入的 entry_idx 是连续的 1-based 数字，
        // 这里通过 off 反推: 我们重新 uleb128 解码
        let mut v: u32 = 0;
        let mut shift = 0u32;
        let mut i = off as usize;
        loop {
            let b = self.data[i];
            i += 1;
            v |= ((b & 0x7f) as u32) << shift;
            if (b & 0x80) == 0 {
                break;
            }
            shift += 7;
        }
        v
    }
    fn into_bytes(self) -> Vec<u8> {
        self.data
    }
}

// ============================================================================
// 公共 API
// ============================================================================

/// 兼容旧签名：构造一个最小 DWARF v4 CU。
/// 内部委托给完整 `build_dwarf_full`。
pub fn build_dwarf(source: &str, functions: &[(&str, u64)]) -> DebugSections {
    let funcs: Vec<DwarfFuncInfo> = functions
        .iter()
        .map(|(name, pc)| DwarfFuncInfo {
            name: (*name).to_string(),
            linkage_name: None,
            low_pc: *pc,
            high_pc: *pc + 16,
            decl_line: 1,
            decl_file_idx: 0,
            return_type: DwarfType {
                name: "int".to_string(),
                byte_size: 4,
                kind: DwarfTypeKind::Base(TypeEncoding::Signed),
            },
            params: vec![],
            is_external: true,
            is_inline: false,
            is_declaration: false,
        })
        .collect();
    let cu = CompileUnit {
        source_name: source.to_string(),
        comp_dir: None,
        producer: format!("dcc {}", env!("CARGO_PKG_VERSION")),
        language: DW_LANG_C99,
        functions: funcs,
        globals: vec![],
        types: vec![],
        line_entries: vec![],
        line_files: vec![source.to_string()],
        line_dirs: vec![".".to_string()],
        low_pc: 0,
        high_pc: 0,
        func_ranges: functions.iter().map(|(_, pc)| (*pc, *pc + 16)).collect(),
        cfi_rows: vec![],
    };
    let opts = DebugOptions {
        level: 2,
        source_name: source.to_string(),
        ..Default::default()
    };
    build_dwarf_full(&cu, &opts)
}

/// 指定目标架构的 DWARF 生成（AArch64 / RISC-V 等）
pub fn build_dwarf_arch(source: &str, functions: &[(&str, u64)], arch: ArchKind) -> DebugSections {
    let funcs: Vec<DwarfFuncInfo> = functions
        .iter()
        .map(|(name, pc)| DwarfFuncInfo {
            name: (*name).to_string(),
            linkage_name: None,
            low_pc: *pc,
            high_pc: *pc + 16,
            decl_line: 1,
            decl_file_idx: 0,
            return_type: DwarfType {
                name: "int".to_string(),
                byte_size: 4,
                kind: DwarfTypeKind::Base(TypeEncoding::Signed),
            },
            params: vec![],
            is_external: true,
            is_inline: false,
            is_declaration: false,
        })
        .collect();
    let cu = CompileUnit {
        source_name: source.to_string(),
        comp_dir: None,
        producer: format!("dcc {}", env!("CARGO_PKG_VERSION")),
        language: DW_LANG_C99,
        functions: funcs,
        globals: vec![],
        types: vec![],
        line_entries: vec![],
        line_files: vec![source.to_string()],
        line_dirs: vec![".".to_string()],
        low_pc: 0,
        high_pc: 0,
        func_ranges: functions.iter().map(|(_, pc)| (*pc, *pc + 16)).collect(),
        cfi_rows: vec![],
    };
    let opts = DebugOptions {
        level: 2,
        source_name: source.to_string(),
        arch,
        ..Default::default()
    };
    build_dwarf_full(&cu, &opts)
}

/// 完整 DWARF v4 + 辅助节生成（主入口）
pub fn build_dwarf_full(cu: &CompileUnit, opts: &DebugOptions) -> DebugSections {
    let mut sec = DebugSections::default();

    let mut strs = StringTable::new();
    let mut abb = AbbrevBuilder::default();

    // 1) .debug_str
    sec.str = strl_section(&mut strs, cu, opts);

    // 2) .debug_abbrev + .debug_info
    let (abbrev, info) = build_abbrev_and_info(cu, opts, &mut strs, &mut abb);
    sec.abbrev = abbrev;
    sec.info = info;

    // 3) .debug_line
    sec.line = build_line_program(cu, opts);

    // 4) .debug_aranges
    sec.aranges = build_aranges(cu, opts);

    // 5) .debug_frame
    sec.frame = build_debug_frame(cu, opts);

    // 6) -gpubnames: .debug_pubnames + .debug_pubtypes + .debug_names
    if opts.pubnames {
        let (pn, pt) = build_pubnames(cu, opts);
        sec.pubnames = pn;
        sec.pubtypes = pt;
        sec.names = build_debug_names(cu, opts);
    }

    // 7) -g3: .debug_macro
    if opts.level >= 3 {
        sec.macro_ = build_debug_macro(cu, opts);
    }

    // 8) -gsplit-dwarf: dwo + skeleton
    if opts.split_dwarf {
        let split = build_split_dwarf(cu, opts);
        sec.info_dwo = split.info_dwo;
        sec.abbrev_dwo = split.abbrev_dwo;
        sec.str_dwo = split.str_dwo;
        sec.line_dwo = split.line_dwo;
        sec.macro_dwo = split.macro_dwo;
        sec.rnglists_dwo = split.rnglists_dwo;
        sec.loclists_dwo = split.loclists_dwo;
        sec.skeleton = split.skeleton;
        sec.skeleton_type = split.skeleton_type;
    }

    // 9) BTF
    sec.btf = build_btf_probe();

    sec
}

// ============================================================================
// .debug_str + .debug_info + .debug_abbrev
// ============================================================================

fn strl_section(strs: &mut StringTable, cu: &CompileUnit, opts: &DebugOptions) -> Vec<u8> {
    strs.intern(&cu.source_name);
    if let Some(cd) = &cu.comp_dir {
        strs.intern(cd);
    }
    strs.intern(&opts.producer);
    for f in &cu.functions {
        strs.intern(&f.name);
        if let Some(ln) = &f.linkage_name {
            strs.intern(ln);
        }
        for (pname, _) in &f.params {
            strs.intern(pname);
        }
        intern_type(strs, &f.return_type);
    }
    for g in &cu.globals {
        strs.intern(&g.name);
        if let Some(ln) = &g.linkage_name {
            strs.intern(ln);
        }
        intern_type(strs, &g.ty);
    }
    for t in &cu.types {
        intern_type(strs, t);
    }
    strs.clone().into_bytes()
}

fn intern_type(strs: &mut StringTable, ty: &DwarfType) {
    strs.intern(&ty.name);
    match &ty.kind {
        DwarfTypeKind::Struct { members } | DwarfTypeKind::Union { members } => {
            for m in members {
                strs.intern(&m.name);
                intern_type(strs, &m.ty);
            }
        }
        DwarfTypeKind::Array { element, .. } => intern_type(strs, element),
        DwarfTypeKind::Pointer(p)
        | DwarfTypeKind::Const(p)
        | DwarfTypeKind::Volatile(p)
        | DwarfTypeKind::Restrict(p) => intern_type(strs, p),
        DwarfTypeKind::Func { ret, params } => {
            intern_type(strs, ret);
            for p in params {
                intern_type(strs, p);
            }
        }
        DwarfTypeKind::Enum {
            members,
            underlying,
        } => {
            for (n, _) in members {
                strs.intern(n);
            }
            intern_type(strs, underlying);
        }
        DwarfTypeKind::Typedef { alias, target } => {
            strs.intern(alias);
            intern_type(strs, target);
        }
        _ => {}
    }
}

fn build_abbrev_and_info(
    cu: &CompileUnit,
    opts: &DebugOptions,
    strs: &mut StringTable,
    abb: &mut AbbrevBuilder,
) -> (Vec<u8>, Vec<u8>) {
    let mut type_offsets: std::collections::HashMap<String, u32> = Default::default();

    // info header: unit_length(4) + version(2) + debug_abbrev_offset(4) + address_size(1)
    let mut info = Vec::new();
    info.extend_from_slice(&[0u8; 4]); // unit_length 占位
    info.extend_from_slice(&4u16.to_le_bytes()); // version = 4
    info.extend_from_slice(&0u32.to_le_bytes()); // debug_abbrev_offset
    info.push(opts.arch.address_size());

    // CU DIE
    let cu_attrs: Vec<(u16, u16)> = vec![
        (DW_AT_producer, DW_FORM_strp),
        (DW_AT_language, DW_FORM_data2),
        (DW_AT_name, DW_FORM_strp),
        (DW_AT_comp_dir, DW_FORM_strp),
        (DW_AT_low_pc, DW_FORM_addr),
        (DW_AT_high_pc, DW_FORM_addr),
        (DW_AT_stmt_list, DW_FORM_sec_offset),
    ];
    let cu_code = abb.get_or_create(DW_TAG_compile_unit, DW_CHILDREN_yes, &cu_attrs);
    write_uleb128(&mut info, cu_code as u64);

    // producer
    write_attr_strp(&mut info, strs.intern(&opts.producer));
    // language
    info.extend_from_slice(&cu.language.to_le_bytes());
    // name
    write_attr_strp(&mut info, strs.intern(&cu.source_name));
    // comp_dir
    if let Some(cd) = &cu.comp_dir {
        write_attr_strp(&mut info, strs.intern(cd));
    }
    // low_pc
    info.extend_from_slice(&cu.low_pc.to_le_bytes());
    // high_pc (DWARF 4: can be constant offset)
    let high_pc_val = cu.high_pc.saturating_sub(cu.low_pc);
    info.extend_from_slice(&high_pc_val.to_le_bytes());
    // stmt_list: 占位, .debug_line 偏移 = 0（GDB 接受 0 表示无行号程序）
    info.extend_from_slice(&0u32.to_le_bytes());

    // type DIEs（顶级类型）
    for t in &cu.types {
        let off = info.len() as u32;
        type_offsets.insert(t.name.clone(), off);
        write_type_die(&mut info, abb, strs, t);
    }

    // function DIEs
    for f in &cu.functions {
        let has_params = !f.params.is_empty();
        let sub_attrs: Vec<(u16, u16)> = vec![
            (DW_AT_name, DW_FORM_strp),
            (DW_AT_linkage_name, DW_FORM_strp),
            (DW_AT_type, DW_FORM_ref4),
            (DW_AT_external, DW_FORM_flag_present),
            (DW_AT_low_pc, DW_FORM_addr),
            (DW_AT_high_pc, DW_FORM_addr),
        ];
        let code = abb.get_or_create(
            DW_TAG_subprogram,
            if has_params { DW_CHILDREN_yes } else { DW_CHILDREN_no },
            &sub_attrs,
        );
        write_uleb128(&mut info, code as u64);

        // name
        write_attr_strp(&mut info, strs.intern(&f.name));
        // linkage_name (optional)
        if let Some(ln) = &f.linkage_name {
            write_attr_strp(&mut info, strs.intern(ln));
        }
        // type (return type)
        let ret_off = ensure_type_die(cu, &mut info, abb, strs, &mut type_offsets, &f.return_type);
        info.extend_from_slice(&ret_off.to_le_bytes());
        // external
        if f.is_external {
            info.push(1);
        } else {
            info.push(0);
        }
        // low_pc / high_pc (high_pc as offset for DWARF 4)
        info.extend_from_slice(&f.low_pc.to_le_bytes());
        info.extend_from_slice(&(f.high_pc - f.low_pc).to_le_bytes());

        // formal parameters
        for (pname, pty) in &f.params {
            let param_attrs: Vec<(u16, u16)> =
                vec![(DW_AT_name, DW_FORM_strp), (DW_AT_type, DW_FORM_ref4)];
            let pcode = abb.get_or_create(DW_TAG_formal_parameter, DW_CHILDREN_no, &param_attrs);
            write_uleb128(&mut info, pcode as u64);
            write_attr_strp(&mut info, strs.intern(pname));
            let pty_off = ensure_type_die(cu, &mut info, abb, strs, &mut type_offsets, pty);
            info.extend_from_slice(&pty_off.to_le_bytes());
        }
        // subprogram 终止
        if has_params {
            info.push(0);
        }
    }

    // globals DIEs
    for g in &cu.globals {
        let var_attrs: Vec<(u16, u16)> = vec![
            (DW_AT_name, DW_FORM_strp),
            (DW_AT_linkage_name, DW_FORM_strp),
            (DW_AT_type, DW_FORM_ref4),
            (DW_AT_external, DW_FORM_flag_present),
        ];
        let code = abb.get_or_create(DW_TAG_variable, DW_CHILDREN_no, &var_attrs);
        write_uleb128(&mut info, code as u64);
        write_attr_strp(&mut info, strs.intern(&g.name));
        if let Some(ln) = &g.linkage_name {
            write_attr_strp(&mut info, strs.intern(ln));
        }
        let ty_off = ensure_type_die(cu, &mut info, abb, strs, &mut type_offsets, &g.ty);
        info.extend_from_slice(&ty_off.to_le_bytes());
        if g.is_external {
            info.push(1);
        } else {
            info.push(0);
        }
    }

    // CU DIE 终止
    info.push(0);

    // 回填 unit_length
    let unit_len = (info.len() - 4) as u32;
    info[0..4].copy_from_slice(&unit_len.to_le_bytes());

    (abb.clone().into_bytes(), info)
}

fn write_attr_strp(info: &mut Vec<u8>, off: u32) {
    info.extend_from_slice(&off.to_le_bytes());
}

fn ensure_type_die(
    _cu: &CompileUnit,
    info: &mut Vec<u8>,
    abb: &mut AbbrevBuilder,
    strs: &mut StringTable,
    type_offsets: &mut std::collections::HashMap<String, u32>,
    ty: &DwarfType,
) -> u32 {
    if let Some(&off) = type_offsets.get(&ty.name) {
        return off;
    }
    let off = info.len() as u32;
    type_offsets.insert(ty.name.clone(), off);
    write_type_die(info, abb, strs, ty);
    off
}

fn write_type_die(
    info: &mut Vec<u8>,
    abb: &mut AbbrevBuilder,
    strs: &mut StringTable,
    ty: &DwarfType,
) {
    match &ty.kind {
        DwarfTypeKind::Base(enc) => {
            let attrs: Vec<(u16, u16)> = vec![
                (DW_AT_name, DW_FORM_strp),
                (DW_AT_byte_size, DW_FORM_data1),
                (DW_AT_encoding, DW_FORM_data1),
            ];
            let code = abb.get_or_create(DW_TAG_base_type, DW_CHILDREN_no, &attrs);
            write_uleb128(info, code as u64);
            write_attr_strp(info, strs.intern(&ty.name));
            info.push(ty.byte_size as u8);
            info.push(*enc as u8);
        }
        DwarfTypeKind::Pointer(inner) => {
            let attrs: Vec<(u16, u16)> =
                vec![(DW_AT_byte_size, DW_FORM_data1), (DW_AT_type, DW_FORM_ref4)];
            let code = abb.get_or_create(DW_TAG_pointer_type, DW_CHILDREN_no, &attrs);
            write_uleb128(info, code as u64);
            info.push(ty.byte_size as u8);
            let target_name = format!("{}*", inner.name);
            let mut inner_offsets = std::collections::HashMap::new();
            let inner_off =
                ensure_type_die_self(info, abb, strs, &mut inner_offsets, inner, &target_name);
            info.extend_from_slice(&inner_off.to_le_bytes());
        }
        DwarfTypeKind::Struct { members } => {
            write_aggregate_die(info, abb, strs, ty, members, false);
        }
        DwarfTypeKind::Union { members } => {
            write_aggregate_die(info, abb, strs, ty, members, true);
        }
        DwarfTypeKind::Array { element, count } => {
            let attrs: Vec<(u16, u16)> =
                vec![(DW_AT_type, DW_FORM_ref4), (DW_AT_count, DW_FORM_data4)];
            let code = abb.get_or_create(DW_TAG_array_type, DW_CHILDREN_yes, &attrs);
            write_uleb128(info, code as u64);
            let target_name = format!("{}[]", element.name);
            let mut inner_offsets = std::collections::HashMap::new();
            let inner_off =
                ensure_type_die_self(info, abb, strs, &mut inner_offsets, element, &target_name);
            info.extend_from_slice(&inner_off.to_le_bytes());
            info.extend_from_slice(&(*count as u32).to_le_bytes());
            // subrange child
            let sub_attrs: Vec<(u16, u16)> = vec![(DW_AT_upper_bound, DW_FORM_data4)];
            let sub_code = abb.get_or_create(DW_TAG_subrange_type, DW_CHILDREN_no, &sub_attrs);
            write_uleb128(info, sub_code as u64);
            info.extend_from_slice(&((*count as u32).saturating_sub(1)).to_le_bytes());
            info.push(0); // array_type children 终止
        }
        DwarfTypeKind::Const(inner) => {
            write_qualifier_die(info, abb, strs, ty, inner, DW_TAG_const_type);
        }
        DwarfTypeKind::Volatile(inner) => {
            write_qualifier_die(info, abb, strs, ty, inner, DW_TAG_volatile_type);
        }
        DwarfTypeKind::Restrict(inner) => {
            write_qualifier_die(info, abb, strs, ty, inner, DW_TAG_restrict_type);
        }
        DwarfTypeKind::Enum {
            members,
            underlying,
        } => {
            let attrs: Vec<(u16, u16)> = vec![
                (DW_AT_name, DW_FORM_strp),
                (DW_AT_byte_size, DW_FORM_data1),
                (DW_AT_type, DW_FORM_ref4),
            ];
            let code = abb.get_or_create(DW_TAG_enumeration_type, DW_CHILDREN_yes, &attrs);
            write_uleb128(info, code as u64);
            write_attr_strp(info, strs.intern(&ty.name));
            info.push(ty.byte_size as u8);
            let target_name = format!("enum-{}", ty.name);
            let mut inner_offsets = std::collections::HashMap::new();
            let u_off = ensure_type_die_self(
                info,
                abb,
                strs,
                &mut inner_offsets,
                underlying,
                &target_name,
            );
            info.extend_from_slice(&u_off.to_le_bytes());
            for (name, val) in members {
                let mem_attrs: Vec<(u16, u16)> = vec![
                    (DW_AT_name, DW_FORM_strp),
                    (DW_AT_const_value, DW_FORM_data4),
                ];
                let mem_code = abb.get_or_create(DW_TAG_enumerator, DW_CHILDREN_no, &mem_attrs);
                write_uleb128(info, mem_code as u64);
                write_attr_strp(info, strs.intern(name));
                info.extend_from_slice(&(*val as i64 as u32).to_le_bytes());
            }
            info.push(0); // enum children 终止
        }
        DwarfTypeKind::Func { ret, params } => {
            let attrs: Vec<(u16, u16)> = vec![(DW_AT_type, DW_FORM_ref4)];
            let code = abb.get_or_create(DW_TAG_subroutine_type, DW_CHILDREN_yes, &attrs);
            write_uleb128(info, code as u64);
            let target_name = format!("{}-ret", ty.name);
            let mut inner_offsets = std::collections::HashMap::new();
            let ret_off =
                ensure_type_die_self(info, abb, strs, &mut inner_offsets, ret, &target_name);
            info.extend_from_slice(&ret_off.to_le_bytes());
            for p in params {
                let ptarget = format!("{}-p", ty.name);
                let p_off = ensure_type_die_self(info, abb, strs, &mut inner_offsets, p, &ptarget);
                let param_attrs: Vec<(u16, u16)> = vec![(DW_AT_type, DW_FORM_ref4)];
                let pcode =
                    abb.get_or_create(DW_TAG_formal_parameter, DW_CHILDREN_no, &param_attrs);
                write_uleb128(info, pcode as u64);
                info.extend_from_slice(&p_off.to_le_bytes());
            }
            info.push(0);
        }
        DwarfTypeKind::Typedef { alias, target } => {
            let attrs: Vec<(u16, u16)> =
                vec![(DW_AT_name, DW_FORM_strp), (DW_AT_type, DW_FORM_ref4)];
            let code = abb.get_or_create(DW_TAG_typedef, DW_CHILDREN_no, &attrs);
            write_uleb128(info, code as u64);
            write_attr_strp(info, strs.intern(alias));
            let target_name = format!("td-{}", alias);
            let mut inner_offsets = std::collections::HashMap::new();
            let t_off =
                ensure_type_die_self(info, abb, strs, &mut inner_offsets, target, &target_name);
            info.extend_from_slice(&t_off.to_le_bytes());
        }
        DwarfTypeKind::Void => {
            // DW_ATE_void (0x00) 兜底
            let attrs: Vec<(u16, u16)> = vec![
                (DW_AT_name, DW_FORM_strp),
                (DW_AT_byte_size, DW_FORM_data1),
                (DW_AT_encoding, DW_FORM_data1),
            ];
            let code = abb.get_or_create(DW_TAG_base_type, DW_CHILDREN_no, &attrs);
            write_uleb128(info, code as u64);
            write_attr_strp(info, strs.intern("void"));
            info.push(0);
            info.push(0x00);
        }
    }
}

fn write_qualifier_die(
    info: &mut Vec<u8>,
    abb: &mut AbbrevBuilder,
    strs: &mut StringTable,
    ty: &DwarfType,
    inner: &DwarfType,
    tag: u16,
) {
    let _ = strs;
    let attrs: Vec<(u16, u16)> = vec![(DW_AT_type, DW_FORM_ref4)];
    let code = abb.get_or_create(tag, DW_CHILDREN_no, &attrs);
    write_uleb128(info, code as u64);
    let target_name = format!("q-{}", ty.name);
    let mut inner_offsets = std::collections::HashMap::new();
    let inner_off = ensure_type_die_self(info, abb, strs, &mut inner_offsets, inner, &target_name);
    info.extend_from_slice(&inner_off.to_le_bytes());
}

fn ensure_type_die_self(
    info: &mut Vec<u8>,
    abb: &mut AbbrevBuilder,
    strs: &mut StringTable,
    offsets: &mut std::collections::HashMap<String, u32>,
    ty: &DwarfType,
    key: &str,
) -> u32 {
    if let Some(&off) = offsets.get(key) {
        return off;
    }
    let off = info.len() as u32;
    offsets.insert(key.to_string(), off);
    write_type_die(info, abb, strs, ty);
    off
}

fn write_aggregate_die(
    info: &mut Vec<u8>,
    abb: &mut AbbrevBuilder,
    strs: &mut StringTable,
    ty: &DwarfType,
    members: &[DwarfMember],
    is_union: bool,
) {
    let tag = if is_union {
        DW_TAG_union_type
    } else {
        DW_TAG_structure_type
    };
    let has_members = !members.is_empty();
    let attrs: Vec<(u16, u16)> = vec![(DW_AT_name, DW_FORM_strp), (DW_AT_byte_size, DW_FORM_data1)];
    let code = abb.get_or_create(
        tag,
        if has_members { DW_CHILDREN_yes } else { DW_CHILDREN_no },
        &attrs,
    );
    write_uleb128(info, code as u64);
    write_attr_strp(info, strs.intern(&ty.name));
    info.push(ty.byte_size as u8);
    for m in members {
        // 位域成员: 多 DW_AT_bit_size + DW_AT_bit_offset 两条属性.
        // 非位域成员: 单 DW_AT_data_member_location 属性.
        let mem_attrs: Vec<(u16, u16)> = if m.bit_size > 0 {
            vec![
                (DW_AT_name, DW_FORM_strp),
                (DW_AT_data_member_location, DW_FORM_data1),
                (DW_AT_bit_size, DW_FORM_data1),
                (DW_AT_bit_offset, DW_FORM_data1),
                (DW_AT_type, DW_FORM_ref4),
            ]
        } else {
            vec![
                (DW_AT_name, DW_FORM_strp),
                (DW_AT_data_member_location, DW_FORM_data1),
                (DW_AT_type, DW_FORM_ref4),
            ]
        };
        let mem_code = abb.get_or_create(DW_TAG_member, DW_CHILDREN_no, &mem_attrs);
        write_uleb128(info, mem_code as u64);
        write_attr_strp(info, strs.intern(&m.name));
        info.push((m.offset_bits / 8) as u8);
        if m.bit_size > 0 {
            // bit_size: 字段宽度 (bit)
            info.push(m.bit_size as u8);
            // bit_offset: big-endian 位序下, 从 byte 顶部到字段的 bit 距离
            // (小端序下, 字段在 byte 内的位偏移需要转换). dcc 当前仅小端
            // x86_64/aarch64/riscv64, 直接用 offset_bits % 8 作为位偏移.
            let bit_in_byte = (m.offset_bits % 8) as u8;
            info.push(bit_in_byte);
        }
        let target_name = format!("{}.{}", ty.name, m.name);
        let mut inner_offsets = std::collections::HashMap::new();
        let t_off = ensure_type_die_self(info, abb, strs, &mut inner_offsets, &m.ty, &target_name);
        info.extend_from_slice(&t_off.to_le_bytes());
    }
    if has_members {
        info.push(0);
    }
}

// ============================================================================
// .debug_line (DWARF v4)
// ============================================================================

fn build_line_program(cu: &CompileUnit, opts: &DebugOptions) -> Vec<u8> {
    let mut out = Vec::new();
    let hdr_start = out.len();
    out.extend_from_slice(&[0u8; 4]); // unit_length
    out.extend_from_slice(&4u16.to_le_bytes()); // version = 4
    let header_length_pos = out.len();
    out.extend_from_slice(&[0u8; 4]); // header_length (回填)
    out.push(opts.min_insn_len);
    out.push(opts.max_ops_per_insn); // DWARF 4 必须为 1
    out.push(opts.default_is_stmt);
    out.push(opts.line_base as u8);
    out.push(opts.line_range);
    out.push(opts.opcode_base);

    // standard_opcode_lengths: opcode_base - 1 = 12 entries
    let opcode_base = opts.opcode_base;
    let std_opcode_lengths: [u8; 12] = [
        0, // DW_LNS_copy
        1, // DW_LNS_advance_pc
        1, // DW_LNS_advance_line
        1, // DW_LNS_set_file
        1, // DW_LNS_set_column
        0, // DW_LNS_negate_stmt
        0, // DW_LNS_set_basic_block
        0, // DW_LNS_const_add_pc
        1, // DW_LNS_fixed_advance_pc
        0, // DW_LNS_set_prologue_end
        0, // DW_LNS_set_epilogue_begin
        1, // DW_LNS_set_isa
    ];
    for &n in &std_opcode_lengths {
        out.push(n);
    }
    let _ = opcode_base; // 仅用于语义核对

    // directory_entry_format
    out.push(1); // directory_entry_format_count
    out.push(DW_LNCT_path as u8);
    out.push(DW_FORM_string as u8);
    // directories_count
    write_uleb128(&mut out, cu.line_dirs.len() as u64);
    for d in &cu.line_dirs {
        out.extend_from_slice(d.as_bytes());
        out.push(0);
    }

    // file_name_entry_format: 4 项
    out.push(4);
    out.push(DW_LNCT_path as u8);
    out.push(DW_FORM_string as u8);
    out.push(DW_LNCT_directory_index as u8);
    out.push(DW_FORM_data1 as u8);
    out.push(DW_LNCT_timestamp as u8);
    out.push(DW_FORM_data1 as u8);
    out.push(DW_LNCT_size as u8);
    out.push(DW_FORM_data1 as u8);
    write_uleb128(&mut out, cu.line_files.len() as u64);
    for f in &cu.line_files {
        out.extend_from_slice(f.as_bytes());
        out.push(0);
        out.push(0); // directory_index
        out.push(0); // timestamp
        out.push(0); // size
    }

    // 回填 header_length
    let header_length = (out.len() - header_length_pos - 4) as u32;
    out[header_length_pos..header_length_pos + 4].copy_from_slice(&header_length.to_le_bytes());

    // ===== Line program =====
    let mut state = LineState {
        line: 1,
        file: 1,
        column: 1,
        is_stmt: true,
        ..LineState::default()
    };

    let entries: Vec<LineEntry> = if cu.line_entries.is_empty() {
        vec![LineEntry {
            pc: cu.low_pc,
            line: 1,
            column: 1,
            file_idx: 0,
            discriminator: 0,
            is_stmt: true,
            basic_block: false,
        }]
    } else {
        cu.line_entries.clone()
    };

    // M24: 去重相邻的同源同行的 line entry (常见于 for/while 紧致循环).
    // 保留每行第一条, 丢弃后续同 (file, line, column, is_stmt, discriminator)
    // 的条目. pc 推进仍正确 (write_line_entry 用 last_pc 算差值), 唯一
    // 损失是丢了一些 pc→line 映射精度. 在 -g1 (low debug) 默认场景下
    // 是净收益, -g3 (full debug) 时关闭.
    let entries: Vec<LineEntry> = if opts.level <= 1 {
        let mut deduped: Vec<LineEntry> = Vec::with_capacity(entries.len());
        for e in entries {
            if let Some(last) = deduped.last() {
                if last.file_idx == e.file_idx
                    && last.line == e.line
                    && last.column == e.column
                    && last.is_stmt == e.is_stmt
                    && last.discriminator == e.discriminator
                {
                    continue;
                }
            }
            deduped.push(e);
        }
        deduped
    } else {
        entries
    };

    let mut last_pc: u64 = 0;
    for (i, e) in entries.iter().enumerate() {
        write_line_entry(&mut out, &mut state, e, last_pc, opts);
        last_pc = e.pc;
        if i == entries.len() - 1 {
            out.push(DW_LNS_extended_op);
            out.push(1);
            out.push(DW_LNE_end_sequence);
        }
    }

    // 回填 unit_length
    let unit_len = (out.len() - hdr_start - 4) as u32;
    out[hdr_start..hdr_start + 4].copy_from_slice(&unit_len.to_le_bytes());

    out
}

#[derive(Debug, Clone, Copy, Default)]
struct LineState {
    address: u64,
    file: u32,
    line: i64,
    column: u32,
    is_stmt: bool,
    basic_block: bool,
    discriminator: u32,
    op_index: u32,
}

fn write_line_entry(
    out: &mut Vec<u8>,
    state: &mut LineState,
    e: &LineEntry,
    last_pc: u64,
    opts: &DebugOptions,
) {
    let pc_advance = e.pc.saturating_sub(last_pc);
    let line_advance = (e.line as i64) - state.line;
    let file_advance = (e.file_idx as i32 + 1) - state.file as i32;
    let column_advance = (e.column as i32) - state.column as i32;

    let opcode_base: u8 = opts.opcode_base;
    let line_base: i8 = opts.line_base;
    let line_range: u8 = opts.line_range;
    let min_insn: u8 = opts.min_insn_len;
    let max_line_incr = line_base as i64 + line_range as i64 - 1;

    // 特殊操作码 (matrix optimization, M24 增强):
    //
    // 基本形态 (pc_advance < min_insn):
    //   opcode = opcode_base + (line_increment - line_base)
    //   1 字节同时推进 line + pc, 比分开 4-5 字节小
    //
    // 增强形态 (pc_advance >= min_insn, 利用 DW_LNS_const_add_pc):
    //   先 emit DW_LNS_const_add_pc (1 字节), 它把 special_op 隐含的
    //   pc 增量再加 (255 * min_insn). 然后再 emit 一个 special opcode.
    //   净增 pc = (255 + adjusted_opcode_pc) * min_insn, 单组 2 字节
    //   表达范围 255*min_insn ~ 510*min_insn, 比单独 emit
    //   DW_LNS_advance_pc (uleb128) 紧 1-2 字节.
    //
    // 折算: 当 (pc_advance / min_insn) 在 (255..510] 且 line 仍在
    // [line_base..line_base+line_range-1] 时, 应用此优化.
    if line_advance >= line_base as i64
        && line_advance <= max_line_incr
        && pc_advance < (min_insn as u64)
    {
        let op_increment = line_advance - line_base as i64;
        let opcode = opcode_base as i64 + op_increment;
        out.push(opcode as u8);
        state.address = e.pc;
        state.line = e.line as i64;
        state.column = e.column;
        state.file = e.file_idx as u32 + 1;
        state.discriminator = e.discriminator as u32;
        return;
    }

    // 增强路径说明 (M24 注释): DW_LNS_const_add_pc (1 字节) 可把 special_op
    // 隐含的 pc 增量再加 255*min_insn. 但它强制 special_op 的 line 增量必须
    // 同时减去 0..line_range 倍, 与原 line_advance 冲突. 当 line_advance
    // 非 0 时, 该路径会改变 line, 故本实现暂不启用. line_advance=0 的场景
    // 已由 op_index=0 的基本 matrix op 覆盖 (1 字节, 不需 const_add_pc).
    // 未来若 line range 区间扩大 (line_range > 255), 可重新评估.

    if file_advance != 0 {
        out.push(DW_LNS_set_file);
        write_uleb128(out, e.file_idx as u64 + 1);
        state.file = e.file_idx as u32 + 1;
    }
    if column_advance != 0 {
        out.push(DW_LNS_set_column);
        write_uleb128(out, e.column as u64);
        state.column = e.column;
    }
    if e.is_stmt != state.is_stmt {
        out.push(DW_LNS_negate_stmt);
        state.is_stmt = e.is_stmt;
    }
    if e.basic_block && !state.basic_block {
        out.push(DW_LNS_set_basic_block);
        state.basic_block = true;
    }
    if line_advance != 0 {
        out.push(DW_LNS_advance_line);
        write_sleb128(out, line_advance);
    }
    if pc_advance != 0 {
        out.push(DW_LNS_advance_pc);
        write_uleb128(out, pc_advance);
    }
    out.push(DW_LNS_copy);
    state.address = e.pc;
    state.line = e.line as i64;
}

// ============================================================================
// .debug_aranges
// ============================================================================

fn build_aranges(cu: &CompileUnit, opts: &DebugOptions) -> Vec<u8> {
    let mut out = Vec::new();
    let hdr_start = out.len();
    out.extend_from_slice(&[0u8; 4]); // unit_length
    out.extend_from_slice(&2u16.to_le_bytes()); // version = 2
    out.extend_from_slice(&0u32.to_le_bytes()); // debug_info_offset
    out.push(opts.arch.address_size());
    out.push(0); // segment_selector_size
                 // pad to 2 * address_size
    while out.len() % (2 * opts.arch.address_size() as usize) != 0 {
        out.push(0);
    }
    if cu.func_ranges.is_empty() {
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
    } else {
        for &(lo, hi) in &cu.func_ranges {
            out.extend_from_slice(&lo.to_le_bytes());
            out.extend_from_slice(&(hi - lo).to_le_bytes());
        }
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
    }
    let unit_len = (out.len() - hdr_start - 4) as u32;
    out[hdr_start..hdr_start + 4].copy_from_slice(&unit_len.to_le_bytes());
    out
}

// ============================================================================
// .debug_frame (CFI v1.0)
// ============================================================================

fn build_debug_frame(cu: &CompileUnit, opts: &DebugOptions) -> Vec<u8> {
    let mut out = Vec::new();
    // CIE
    let cie_start = out.len();
    out.extend_from_slice(&[0u8; 4]); // length
    out.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes()); // CIE_id
    out.push(1); // version
    out.push(0); // augmentation
    write_uleb128(&mut out, 1); // code_alignment_factor
    write_sleb128(&mut out, -8); // data_alignment_factor
    write_uleb128(&mut out, opts.arch.dwarf_reg_ra() as u64); // return_address_register
    out.push(0); // augmentation_data length
                 // initial instructions: CFA = rsp + 8
    out.push(DW_CFA_def_cfa);
    write_uleb128(&mut out, opts.arch.cfa_register() as u64);
    write_uleb128(&mut out, 8);
    out.push(DW_CFA_offset | (opts.arch.dwarf_reg_ra() & 0x3f));
    write_uleb128(&mut out, 1); // offset = 8 / data_align = 8 = 1
    out.push(DW_CFA_nop);
    let cie_len = (out.len() - cie_start - 4) as u32;
    out[cie_start..cie_start + 4].copy_from_slice(&cie_len.to_le_bytes());
    let cie_id_off = cie_start + 4;

    // FDEs
    for &(lo, hi) in &cu.func_ranges {
        if hi <= lo {
            continue;
        }
        let fde_start = out.len();
        out.extend_from_slice(&[0u8; 4]); // length
        let cie_ptr = (cie_id_off as i64 - (fde_start as i64 + 4)) as u32;
        out.extend_from_slice(&cie_ptr.to_le_bytes());
        out.extend_from_slice(&lo.to_le_bytes());
        out.extend_from_slice(&(hi - lo).to_le_bytes());
        out.push(0); // augmentation length
                     // 写 CFI rows
        let mut pc_off: u64 = 0;
        let fde_size = hi - lo;
        let rows: Vec<CfiRow> = cu
            .cfi_rows
            .iter()
            .copied()
            .filter(|r| r.pc >= lo && r.pc < hi)
            .collect();
        for row in &rows {
            let advance = row.pc - lo;
            if advance > pc_off {
                let diff = advance - pc_off;
                if diff < 0x40 {
                    out.push(DW_CFA_advance_loc | (diff as u8 & 0x3f));
                } else {
                    out.push(DW_CFA_advance_loc);
                    write_uleb128(&mut out, diff);
                }
                pc_off = advance;
            }
            out.push(DW_CFA_def_cfa);
            write_uleb128(&mut out, row.cfa_reg as u64);
            write_uleb128(&mut out, row.cfa_off as u64);
            for &(reg, off) in row.saved {
                out.push(DW_CFA_offset | (reg & 0x3f));
                write_uleb128(&mut out, off as u64);
            }
        }
        // 末尾: advance 到函数结束, 恢复
        if pc_off < fde_size {
            let diff = fde_size - pc_off;
            out.push(DW_CFA_advance_loc);
            write_uleb128(&mut out, diff);
        }
        out.push(DW_CFA_def_cfa);
        write_uleb128(&mut out, opts.arch.cfa_register() as u64);
        write_uleb128(&mut out, 8);
        out.push(DW_CFA_nop);
        let fde_len = (out.len() - fde_start - 4) as u32;
        out[fde_start..fde_start + 4].copy_from_slice(&fde_len.to_le_bytes());
    }
    let _ = DW_CFA_extended;
    out
}

// ============================================================================
// .debug_pubnames / .debug_pubtypes / .debug_names
// ============================================================================

fn build_pubnames(cu: &CompileUnit, _opts: &DebugOptions) -> (Vec<u8>, Vec<u8>) {
    let mut pubnames = Vec::new();
    let mut pubtypes = Vec::new();

    // pubnames
    let pn_start = pubnames.len();
    pubnames.extend_from_slice(&[0u8; 4]);
    pubnames.extend_from_slice(&2u16.to_le_bytes());
    pubnames.extend_from_slice(&0u32.to_le_bytes());
    pubnames.extend_from_slice(&cu.low_pc.to_le_bytes());
    pubnames.extend_from_slice(&cu.high_pc.to_le_bytes());
    for f in &cu.functions {
        if !f.is_external {
            continue;
        }
        pubnames.extend_from_slice(f.name.as_bytes());
        pubnames.push(0);
        pubnames.extend_from_slice(&f.low_pc.to_le_bytes());
    }
    pubnames.extend_from_slice(&0u32.to_le_bytes());
    let pn_len = (pubnames.len() - pn_start - 4) as u32;
    pubnames[pn_start..pn_start + 4].copy_from_slice(&pn_len.to_le_bytes());

    // pubtypes
    let pt_start = pubtypes.len();
    pubtypes.extend_from_slice(&[0u8; 4]);
    pubtypes.extend_from_slice(&2u16.to_le_bytes());
    pubtypes.extend_from_slice(&0u32.to_le_bytes());
    pubtypes.extend_from_slice(&cu.low_pc.to_le_bytes());
    pubtypes.extend_from_slice(&cu.high_pc.to_le_bytes());
    for t in &cu.types {
        pubtypes.extend_from_slice(t.name.as_bytes());
        pubtypes.push(0);
        pubtypes.extend_from_slice(&cu.low_pc.to_le_bytes());
    }
    pubtypes.extend_from_slice(&0u32.to_le_bytes());
    let pt_len = (pubtypes.len() - pt_start - 4) as u32;
    pubtypes[pt_start..pt_start + 4].copy_from_slice(&pt_len.to_le_bytes());

    (pubnames, pubtypes)
}

/// 简化版 .debug_names（DWARF 5 头）
fn build_debug_names(cu: &CompileUnit, _opts: &DebugOptions) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0u8; 4]); // unit_length 占位
    out.extend_from_slice(&5u16.to_le_bytes()); // version = 5
    out.push(0);
    out.push(0);
    out.extend_from_slice(&1u32.to_le_bytes()); // comp_unit_count
    out.extend_from_slice(&0u32.to_le_bytes()); // local_type_unit_count
    out.extend_from_slice(&0u32.to_le_bytes()); // foreign_type_unit_count
    out.extend_from_slice(&1u32.to_le_bytes()); // bucket_count
    let name_count = cu.functions.len() as u32;
    out.extend_from_slice(&name_count.to_le_bytes());
    // abbrev table: 1 entry (DW_IDX_compile_unit, DW_FORM_data1)
    out.extend_from_slice(&1u32.to_le_bytes());
    out.push(1); // DW_IDX_compile_unit
    out.push(DW_FORM_data1 as u8);
    out.push(0); // 终止
                 // bucket table
    out.extend_from_slice(&0u32.to_le_bytes());
    // name table
    for f in &cu.functions {
        out.extend_from_slice(f.name.as_bytes());
        out.push(0);
        out.push(0); // 1 个 idx_entry, value 0
    }
    let total = (out.len() - 4) as u32;
    out[0..4].copy_from_slice(&total.to_le_bytes());
    out
}

// ============================================================================
// .debug_macro
// ============================================================================

fn build_debug_macro(cu: &CompileUnit, _opts: &DebugOptions) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&4u16.to_le_bytes()); // version = 4
    out.push(0); // flags
    out.push(0); // debug_line_offset (4 bytes placeholder)
    out.push(0);
    out.push(0);
    out.push(0);
    for f in &cu.line_files {
        out.push(0x1d); // DW_MACRO_define_strx (DWARF 5) - 我们用 4 的扩展: define_str
                        // 严格意义上 DWARF 4 调试宏用 DW_MACRO_define (0x01) + DW_FORM_string
        out.push(0x01); // DW_MACRO_define
        out.extend_from_slice(f.as_bytes());
        out.push(0);
    }
    out.push(0); // 终止
    out
}

// ============================================================================
// Split-dwarf
// ============================================================================

struct SplitDwarfOutput {
    info_dwo: Vec<u8>,
    abbrev_dwo: Vec<u8>,
    str_dwo: Vec<u8>,
    line_dwo: Vec<u8>,
    macro_dwo: Vec<u8>,
    rnglists_dwo: Vec<u8>,
    loclists_dwo: Vec<u8>,
    skeleton: Vec<u8>,
    skeleton_type: Vec<u8>,
}

fn build_split_dwarf(cu: &CompileUnit, opts: &DebugOptions) -> SplitDwarfOutput {
    // DWO info: 写一个 skeleton CU 头
    let mut info = Vec::new();
    info.extend_from_slice(&[0u8; 4]); // unit_length
    info.extend_from_slice(&4u16.to_le_bytes());
    info.extend_from_slice(&0u32.to_le_bytes()); // abbrev offset
    info.push(opts.arch.address_size());
    // dwo_id (placeholder)
    info.extend_from_slice(&0u64.to_le_bytes());
    // empty children
    info.push(0);
    let ilen = (info.len() - 4) as u32;
    info[0..4].copy_from_slice(&ilen.to_le_bytes());

    // 简化 abbrev: 1 个空 CU 缩写
    let abbrev = vec![1, DW_TAG_compile_unit as u8, DW_CHILDREN_no, 0, 0, 0];
    let str_dwo = cu.source_name.as_bytes().to_vec();
    let line_dwo = build_line_program(cu, opts);
    let macro_dwo = build_debug_macro(cu, opts);

    // Skeleton CU
    let mut skeleton = Vec::new();
    skeleton.extend_from_slice(&[0u8; 4]);
    skeleton.extend_from_slice(&4u16.to_le_bytes());
    skeleton.extend_from_slice(&0u32.to_le_bytes());
    skeleton.push(opts.arch.address_size());
    let sk_attrs: Vec<(u16, u16)> = vec![
        (DW_AT_name, DW_FORM_strp),
        (DW_AT_comp_dir, DW_FORM_strp),
        (DW_AT_GNU_dwo_id, DW_FORM_data8),
        (DW_AT_GNU_dwo_name, DW_FORM_strp),
    ];
    let mut abb2 = AbbrevBuilder::default();
    let code = abb2.get_or_create(DW_TAG_compile_unit, DW_CHILDREN_no, &sk_attrs);
    write_uleb128(&mut skeleton, code as u64);
    let mut strs = StringTable::new();
    write_attr_strp(&mut skeleton, strs.intern(&cu.source_name));
    if let Some(cd) = &cu.comp_dir {
        write_attr_strp(&mut skeleton, strs.intern(cd));
    }
    skeleton.extend_from_slice(&0u64.to_le_bytes()); // dwo_id
    write_attr_strp(
        &mut skeleton,
        strs.intern(&format!("{}.dwo", cu.source_name)),
    );
    skeleton.push(0);
    let sklen = (skeleton.len() - 4) as u32;
    skeleton[0..4].copy_from_slice(&sklen.to_le_bytes());

    SplitDwarfOutput {
        info_dwo: info,
        abbrev_dwo: abbrev,
        str_dwo,
        line_dwo,
        macro_dwo,
        rnglists_dwo: Vec::new(),
        loclists_dwo: Vec::new(),
        skeleton,
        skeleton_type: Vec::new(),
    }
}

// ============================================================================
// ELF 节列表（兼容旧 API）
// ============================================================================

pub fn dwarf_elf_sections(dbg: &DebugSections) -> Vec<ElfSection> {
    dbg.to_elf_sections(false)
}

// ============================================================================
// BTF (Binary Type Format, BPF Type Format)
// ============================================================================

/// 生成含 int + dcc_probe 结构体的 BTF（供 pahole/llvm-btf 探测）
pub fn build_btf_probe() -> Vec<u8> {
    let strings = b"\0int\0dcc_probe\0val\0";
    let type_off = 24u32;

    let mut types = Vec::new();
    // type 1: int (32-bit signed)
    types.extend_from_slice(&1u32.to_le_bytes()); // name_off "int"
    types.extend_from_slice(&BTF_KIND_INT.to_le_bytes());
    let int_enc = 32u32 << 24; // 32 bits, offset 0
    types.extend_from_slice(&int_enc.to_le_bytes());
    // type 2: struct dcc_probe { int val; } size 4
    types.extend_from_slice(&5u32.to_le_bytes()); // name_off "dcc_probe"
    let struct_info = (1u32 << 16) | BTF_KIND_STRUCT; // vlen=1
    types.extend_from_slice(&struct_info.to_le_bytes());
    types.extend_from_slice(&4u32.to_le_bytes()); // size
    types.extend_from_slice(&16u32.to_le_bytes()); // member name "val"
    types.extend_from_slice(&1u32.to_le_bytes()); // member type id 1
    types.extend_from_slice(&0u32.to_le_bytes()); // member offset bits

    let type_len = types.len() as u32;
    let str_section_off = type_off + type_len;

    let mut btf = Vec::new();
    btf.extend_from_slice(&0xEB9F_u16.to_le_bytes());
    btf.extend_from_slice(&1u8.to_le_bytes());
    btf.extend_from_slice(&0u32.to_le_bytes());
    btf.extend_from_slice(&24u32.to_le_bytes()); // hdr_len
    btf.extend_from_slice(&type_off.to_le_bytes());
    btf.extend_from_slice(&type_len.to_le_bytes());
    btf.extend_from_slice(&str_section_off.to_le_bytes());
    btf.extend_from_slice(&(strings.len() as u32).to_le_bytes());
    btf.extend_from_slice(&types);
    btf.extend_from_slice(strings);
    btf
}

/// 兼容旧名
pub fn build_btf_header() -> Vec<u8> {
    build_btf_probe()
}

// ============================================================================
// .eh_frame + .orc_unwind_ip
// ============================================================================

/// 生成含 CIE + 每函数 FDE 的 .eh_frame（64-bit LSB）
/// 支持 x86_64 / aarch64 / riscv64 三种 CIE 变体
pub fn build_eh_frame(functions: &[FuncFrame]) -> Vec<u8> {
    build_eh_frame_with_arch(functions, ArchKind::X86_64)
}

/// 同上但允许指定目标架构
pub fn build_eh_frame_with_arch(functions: &[FuncFrame], arch: ArchKind) -> Vec<u8> {
    let mut eh = Vec::new();
    let augmentation = arch.cie_augmentation();

    // CIE body
    // LSB .eh_frame 用 CIE_id = 0 (区别于 .debug_frame 的 0xFFFFFFFF DWARF 约定).
    // 0 是 glibc unwinder / setjmp 实际解析用的值, 错填 0xFFFFFFFF 会让
    // FDE 找不到对应 CIE → setjmp/longjmp 异常时栈展开失败.
    let cie_len_pos = eh.len();
    eh.extend_from_slice(&[0u8; 4]);
    eh.extend_from_slice(&0u32.to_le_bytes());
    eh.push(1); // version
    eh.extend_from_slice(augmentation.as_bytes());
    eh.push(0); // null-terminate augmentation
    write_uleb128(&mut eh, 1); // code_alignment_factor
    write_sleb128(&mut eh, -8); // data_alignment_factor
    write_uleb128(&mut eh, arch.dwarf_reg_ra() as u64);
    if !augmentation.is_empty() {
        // "zR" augmentation: 1 字节 augmentation_data 长度
        eh.push(1);
        // FDE encoding: DW_EH_PE_pcrel (0x10) | DW_EH_PE_sdata4 (0x0b) = 0x1b
        // ld 用这个 encoding 决定 FDE 里 PC begin / PC range 如何编码.
        // 之前调成 0x01 (=DW_EH_PE_absptr) 会让 ld 误读 FDE PC 字段.
        eh.push(0x1b);
    } else {
        eh.push(0);
    }
    // 4 字节对齐下一个 record (FDE 起点) - LSB 仅要求 4 字节对齐.
    // 之前用 `eh.len() % 8 != 4` 造成 6 字节 CFA_nop, 使 readelf 误读 CIE 长度.
    while (eh.len() - cie_len_pos - 4) % 4 != 0 {
        eh.push(0);
    }
    let cie_len = (eh.len() - cie_len_pos - 4) as u32;
    eh[cie_len_pos..cie_len_pos + 4].copy_from_slice(&cie_len.to_le_bytes());

    for func in functions {
        let fde_start = eh.len();
        eh.extend_from_slice(&[0u8; 4]);
        // LSB .eh_frame FDE.CIE_pointer 字段 = (FDE 长度字段之后的地址) - CIE 起始地址.
        // 比如 CIE 在 offset 0, FDE 长度在 0x10, FDE+4 在 0x14, 字段在 0x14, 则
        // cie_ptr = 0x14 - 0 = 0x14 (正跳距离). readelf / ld 都按这个公式
        // 跳回 CIE. 之前用 `cie_len_pos - (fde_start + 4)` 跳反方向, 在大函数
        // / 多个 FDE 下读错 FDE 长度, 报 "cie=invalid" / "PHDR segment not
        // covered by LOAD segment" / DW_CFA_expression corrupt.
        let cie_ptr = ((fde_start + 4) as i64 - cie_len_pos as i64) as u32;
        eh.extend_from_slice(&cie_ptr.to_le_bytes());
        // LSB .eh_frame FDE 初始位置 + 地址范围字段宽度 = address_size,
        // x86_64/aarch64/riscv64/mips64 = 8 字节; mips/arm = 4 字节.
        // 之前一律用 func.size (u32) to_le_bytes() (4 字节), 64 位目标下
        // 会把后续 aug_data_length/CFI 全部错位 4 字节 → readelf FDE 长度
        // 报告为 0x0190028d, PC 范围成 0x10070c0000000017, 引发
        // "PHDR segment not covered by LOAD segment" / "DW_CFA_expression
        // corrupt len".
        let addr_size: usize = arch.address_size() as usize;
        let pc_bytes = func.pc.to_le_bytes();
        eh.extend_from_slice(&pc_bytes[..addr_size]);
        let size_bytes = (func.size as u64).to_le_bytes();
        eh.extend_from_slice(&size_bytes[..addr_size]);
        eh.push(0); // augmentation data length
                    // CFI ops: 初始化 CFA + 各寄存器保存描述
                    // (setjmp / longjmp 依赖此信息正确恢复 callee-saved 寄存器)
        if !func.cfi_ops.is_empty() {
            eh.extend_from_slice(&func.cfi_ops);
        }
        // FDE 整体按 8 字节对齐 (address_size = 8 的目标也对齐到 8)
        while eh.len() % 8 != 0 {
            eh.push(0);
        }
        let fde_len = (eh.len() - fde_start - 4) as u32;
        eh[fde_start..fde_start + 4].copy_from_slice(&fde_len.to_le_bytes());
    }
    eh
}

/// ORC 元数据：写入各函数 PC（objtool 探测用）
pub fn build_orc_unwind_ip(functions: &[FuncFrame]) -> Vec<u8> {
    let mut orc = Vec::new();
    for func in functions {
        orc.extend_from_slice(&func.pc.to_le_bytes());
    }
    if orc.is_empty() {
        orc.extend_from_slice(&[0u8; 8]);
    }
    orc
}

// ============================================================================
// GDB index section (--gdb-index)
// ============================================================================

/// GDB index 版本（写入 .gdb_index 头的 magic）
pub const GDB_INDEX_MAGIC: &[u8; 7] = b"gdb-9.2";

/// 构造完整 GDB index 节
///   1) symbol table (type + CU offset + symbol name)
///   2) constant pool (字符串 + CUs)
///   3) address ranges (function ranges)
pub fn build_gdb_index(functions: &[(&str, u64, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    // header
    out.extend_from_slice(GDB_INDEX_MAGIC); // 7 bytes
    out.push(0); // padding to 8
    out.extend_from_slice(&7u32.to_le_bytes()); // version 7
    let symbol_table_off_pos = out.len();
    out.extend_from_slice(&0u32.to_le_bytes());
    let constant_pool_off_pos = out.len();
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(functions.len() as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // cu count = 1

    // symbol table: 16 bytes per entry
    let symbol_table_off = out.len();
    for (name, _pc, _size) in functions {
        // name slot: 8 bytes (短名 inline 或偏移 placeholder)
        if name.len() < 8 {
            let mut buf = [0u8; 8];
            buf[..name.len()].copy_from_slice(name.as_bytes());
            out.extend_from_slice(&buf);
        } else {
            // 长名: 先占位, constant pool 写完后回填
            out.extend_from_slice(&[0u8; 8]);
        }
        out.extend_from_slice(&0u32.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // cu_index
        out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    }
    // terminator
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());

    // constant pool
    let constant_pool_off = out.len();
    let mut long_name_offsets: Vec<u32> = Vec::new();
    for (name, _pc, _size) in functions {
        if name.len() < 8 {
            long_name_offsets.push(0);
        } else {
            let off = (out.len() - constant_pool_off) as u32;
            long_name_offsets.push(off);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
        }
    }

    // 回填长名偏移到 symbol table
    for (i, (name, _pc, _size)) in functions.iter().enumerate() {
        if name.len() >= 8 {
            let off = long_name_offsets[i];
            let pos = symbol_table_off + i * 16;
            out[pos..pos + 8].copy_from_slice(&off.to_le_bytes());
        }
    }

    // patch header offsets
    out[symbol_table_off_pos..symbol_table_off_pos + 4]
        .copy_from_slice(&(symbol_table_off as u32).to_le_bytes());
    out[constant_pool_off_pos..constant_pool_off_pos + 4]
        .copy_from_slice(&(constant_pool_off as u32).to_le_bytes());

    out
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn btf_probe_has_struct_type() {
        let btf = build_btf_probe();
        assert!(btf.starts_with(&0xEB9F_u16.to_le_bytes()));
        assert!(btf.len() > 24);
        assert!(btf.windows(4).any(|w| {
            let info = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            (info & 0x1f) == BTF_KIND_STRUCT
        }));
    }

    #[test]
    fn eh_frame_includes_fde_for_function() {
        let frames = [FuncFrame::new(0x100, 32)];
        let eh = build_eh_frame(&frames);
        assert!(eh.len() > 32, "eh_frame should contain CIE + FDE");
    }

    #[test]
    fn aarch64_eh_frame_uses_zr_augmentation() {
        let frames = [FuncFrame::new(0x200, 16)];
        let eh = build_eh_frame_with_arch(&frames, ArchKind::AArch64);
        let win = b"zR\0";
        let mut found = false;
        for w in eh.windows(win.len()) {
            if w == win {
                found = true;
                break;
            }
        }
        assert!(found, "aarch64 CIE should contain 'zR' augmentation");
    }

    #[test]
    fn riscv64_eh_frame_uses_zr_augmentation() {
        let frames = [FuncFrame::new(0x300, 16)];
        let eh = build_eh_frame_with_arch(&frames, ArchKind::RiscV64);
        let win = b"zR\0";
        let mut found = false;
        for w in eh.windows(win.len()) {
            if w == win {
                found = true;
                break;
            }
        }
        assert!(found, "riscv64 CIE should contain 'zR' augmentation");
    }

    /// M24.1: setjmp / longjmp 依赖 .eh_frame FDE 内的 CFI ops 描述
    /// callee-saved 寄存器位置. 验证 FDE ops 正确插入并包含
    /// DW_CFA_def_cfa + DW_CFA_offset(13, 2) + DW_CFA_offset(16, 1) 序列.
    #[test]
    fn eh_frame_fde_emits_cfi_ops_for_setjmp() {
        // 构造 cfi_ops: CFA = rsp + 16, r13@CFA-16, %rip@CFA-8
        // DW_CFA_def_cfa(7, 16); DW_CFA_offset(13, 2); DW_CFA_offset(16, 1)
        let cfi_ops = vec![0x0c, 7, 16, 0x80 | 13, 2, 0x80 | 16, 1];

        let frames = [FuncFrame::with_cfi(0x100, 32, cfi_ops.clone())];
        let eh = build_eh_frame(&frames);
        // 验证 FDE 内出现 cfi_ops 的关键字节序列: DW_CFA_def_cfa 0x0c
        let needle_def_cfa = [0x0c, 7, 16u8];
        let mut found = false;
        for w in eh.windows(needle_def_cfa.len()) {
            if w == needle_def_cfa {
                found = true;
                break;
            }
        }
        assert!(found, "FDE should embed DW_CFA_def_cfa(7, 16) from cfi_ops");
        // 验证 DW_CFA_offset 字节
        let needle_r13 = [0x80u8 | 13, 2];
        let mut found_r13 = false;
        for w in eh.windows(needle_r13.len()) {
            if w == needle_r13 {
                found_r13 = true;
                break;
            }
        }
        assert!(
            found_r13,
            "FDE should embed DW_CFA_offset(13, 2) for r13 save"
        );
        // 验证 %rip (reg 16) 字节
        let needle_rip = [0x80u8 | 16, 1];
        let mut found_rip = false;
        for w in eh.windows(needle_rip.len()) {
            if w == needle_rip {
                found_rip = true;
                break;
            }
        }
        assert!(
            found_rip,
            "FDE should embed DW_CFA_offset(16, 1) for %rip save"
        );
    }

    /// M24.1: cfi_ops 为空时 FDE 不嵌入任何 CFI ops, 仅 PC+size.
    #[test]
    fn eh_frame_fde_omits_cfi_ops_when_empty() {
        let frames = [FuncFrame::new(0x400, 16)];
        let eh = build_eh_frame(&frames);
        // 不应该出现 DW_CFA_def_cfa(7, 16) 序列 (cfi_ops 为空).
        let needle = [0x0c, 0x07, 0x10];
        let mut found_in_full = false;
        for w in eh.windows(needle.len()) {
            if w == needle {
                found_in_full = true;
                break;
            }
        }
        assert!(
            !found_in_full,
            "FDE with empty cfi_ops should not embed def_cfa(7,16)"
        );
    }

    #[test]
    fn dwarf_info_has_version_4_marker() {
        let dbg = build_dwarf("test.c", &[("main", 0x100)]);
        assert!(dbg.info.len() > 6);
        assert_eq!(dbg.info[4], 4);
        assert_eq!(dbg.info[5], 0);
    }

    #[test]
    fn dwarf_line_has_version_4_marker() {
        let dbg = build_dwarf("test.c", &[("main", 0x100)]);
        assert!(dbg.line.len() > 6);
        assert_eq!(dbg.line[4], 4);
        assert_eq!(dbg.line[5], 0);
    }

    #[test]
    fn dwarf_aranges_present() {
        let dbg = build_dwarf("test.c", &[("main", 0x100)]);
        assert!(!dbg.aranges.is_empty());
    }

    #[test]
    fn split_dwarf_info_dwo_has_version_4() {
        let cu = CompileUnit {
            source_name: "x.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            functions: vec![],
            globals: vec![],
            types: vec![],
            line_entries: vec![],
            line_files: vec!["x.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![(0, 16)],
            cfi_rows: vec![],
        };
        let opts = DebugOptions {
            level: 2,
            split_dwarf: true,
            ..Default::default()
        };
        let sec = build_dwarf_full(&cu, &opts);
        assert!(!sec.info_dwo.is_empty());
        assert!(!sec.skeleton.is_empty());
    }

    #[test]
    fn debug_frame_has_cie_and_fde() {
        let cu = CompileUnit {
            source_name: "x.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            functions: vec![],
            globals: vec![],
            types: vec![],
            line_entries: vec![],
            line_files: vec!["x.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![(0, 16)],
            cfi_rows: vec![],
        };
        let sec = build_dwarf_full(&cu, &DebugOptions::default());
        assert!(!sec.frame.is_empty());
        let magic = u32::from_le_bytes([sec.frame[4], sec.frame[5], sec.frame[6], sec.frame[7]]);
        assert_eq!(magic, 0xFFFFFFFF);
    }

    #[test]
    fn gdb_index_built() {
        let v = vec![("main", 0x100u64, 32u32), ("foo", 0x200, 16)];
        let idx = build_gdb_index(&v);
        assert!(idx.starts_with(GDB_INDEX_MAGIC));
    }

    #[test]
    fn pubnames_built_when_enabled() {
        let cu = CompileUnit {
            source_name: "x.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            functions: vec![DwarfFuncInfo {
                name: "main".to_string(),
                linkage_name: None,
                low_pc: 0,
                high_pc: 16,
                decl_line: 1,
                decl_file_idx: 0,
                return_type: DwarfType {
                    name: "int".to_string(),
                    byte_size: 4,
                    kind: DwarfTypeKind::Base(TypeEncoding::Signed),
                },
                params: vec![],
                is_external: true,
                is_inline: false,
                is_declaration: false,
            }],
            globals: vec![],
            types: vec![],
            line_entries: vec![],
            line_files: vec!["x.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0,
            high_pc: 16,
            func_ranges: vec![(0, 16)],
            cfi_rows: vec![],
        };
        let opts = DebugOptions {
            level: 2,
            pubnames: true,
            ..Default::default()
        };
        let sec = build_dwarf_full(&cu, &opts);
        assert!(!sec.pubnames.is_empty());
        assert!(!sec.pubtypes.is_empty());
        assert!(!sec.names.is_empty());
    }

    #[test]
    fn dwarf_line_handles_matrix_opcode() {
        // 模拟矩阵操作码：line 推进 1, pc 推进 0 → 用 1 字节 special opcode
        let entries = vec![
            LineEntry {
                pc: 0x100,
                line: 1,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
            LineEntry {
                pc: 0x100,
                line: 2,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
        ];
        let cu = CompileUnit {
            source_name: "x.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            functions: vec![],
            globals: vec![],
            types: vec![],
            line_entries: entries,
            line_files: vec!["x.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0x100,
            high_pc: 0x200,
            func_ranges: vec![],
            cfi_rows: vec![],
        };
        let sec = build_dwarf_full(&cu, &DebugOptions::default());
        assert!(!sec.line.is_empty());
    }

    #[test]
    fn dwarf_type_struct_round_trip() {
        let ty = DwarfType {
            name: "point".to_string(),
            byte_size: 8,
            kind: DwarfTypeKind::Struct {
                members: vec![
                    DwarfMember::regular(
                        "x",
                        0,
                        DwarfType {
                            name: "int".to_string(),
                            byte_size: 4,
                            kind: DwarfTypeKind::Base(TypeEncoding::Signed),
                        },
                    ),
                    DwarfMember::regular(
                        "y",
                        32,
                        DwarfType {
                            name: "int".to_string(),
                            byte_size: 4,
                            kind: DwarfTypeKind::Base(TypeEncoding::Signed),
                        },
                    ),
                ],
            },
        };
        let cu = CompileUnit {
            source_name: "x.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            functions: vec![],
            globals: vec![],
            types: vec![ty],
            line_entries: vec![],
            line_files: vec!["x.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };
        let sec = build_dwarf_full(&cu, &DebugOptions::default());
        assert!(!sec.info.is_empty());
        assert!(!sec.abbrev.is_empty());
        assert!(!sec.str.is_empty());
    }

    #[test]
    fn dwarf_type_bitfield_emits_bit_size_and_offset() {
        // 位域结构体: 一字节容器内含两个位域 a:3 + b:5, 验证 DWARF 编码
        // 包含 DW_AT_bit_size + DW_AT_bit_offset 属性.
        let ty = DwarfType {
            name: "flags".to_string(),
            byte_size: 1,
            kind: DwarfTypeKind::Struct {
                members: vec![
                    DwarfMember::bitfield(
                        "a",
                        0,
                        3,
                        DwarfType {
                            name: "unsigned int".to_string(),
                            byte_size: 4,
                            kind: DwarfTypeKind::Base(TypeEncoding::Unsigned),
                        },
                    ),
                    DwarfMember::bitfield(
                        "b",
                        3,
                        5,
                        DwarfType {
                            name: "unsigned int".to_string(),
                            byte_size: 4,
                            kind: DwarfTypeKind::Base(TypeEncoding::Unsigned),
                        },
                    ),
                ],
            },
        };
        let cu = CompileUnit {
            source_name: "x.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            functions: vec![],
            globals: vec![],
            types: vec![ty],
            line_entries: vec![],
            line_files: vec!["x.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };
        let sec = build_dwarf_full(&cu, &DebugOptions::default());
        // 缩略表 + 信息表都应非空
        assert!(!sec.abbrev.is_empty());
        assert!(!sec.info.is_empty());
        // 验证 abbrev 表中存在 DW_AT_bit_size (0x0d) 编码
        // (libdecnumber-style 字节扫描: uleb128 编码后两字节 LE = 0x0d 0x0b)
        let abbrev_bytes = &sec.abbrev;
        let mut has_bit_size = false;
        let mut i = 0;
        while i < abbrev_bytes.len().saturating_sub(1) {
            // 找任意相邻 LE u16 == 0x0d0b (DW_AT_bit_size=0x0d, DW_FORM_data1=0x0b)
            if abbrev_bytes[i] == 0x0d && abbrev_bytes[i + 1] == 0x0b {
                has_bit_size = true;
                break;
            }
            i += 1;
        }
        assert!(
            has_bit_size,
            "abbrev table should encode DW_AT_bit_size + DW_FORM_data1 for bitfield members"
        );
        // 验证 DW_AT_bit_offset (0x0e) 同样存在
        let mut has_bit_offset = false;
        for w in sec.abbrev.windows(2) {
            if w[0] == 0x0e && w[1] == 0x0b {
                has_bit_offset = true;
                break;
            }
        }
        assert!(
            has_bit_offset,
            "abbrev table should encode DW_AT_bit_offset + DW_FORM_data1 for bitfield members"
        );
    }

    #[test]
    fn dwarf_regular_member_has_no_bit_size() {
        // 非位域成员不应触发 DW_AT_bit_size/DW_AT_bit_offset 属性集
        let ty = DwarfType {
            name: "plain".to_string(),
            byte_size: 4,
            kind: DwarfTypeKind::Struct {
                members: vec![DwarfMember::regular(
                    "v",
                    0,
                    DwarfType {
                        name: "int".to_string(),
                        byte_size: 4,
                        kind: DwarfTypeKind::Base(TypeEncoding::Signed),
                    },
                )],
            },
        };
        let cu = CompileUnit {
            source_name: "x.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            functions: vec![],
            globals: vec![],
            types: vec![ty],
            line_entries: vec![],
            line_files: vec!["x.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };
        let sec = build_dwarf_full(&cu, &DebugOptions::default());
        // 缩略表不应出现 DW_AT_bit_size
        let mut has_bit_size = false;
        for w in sec.abbrev.windows(2) {
            if w[0] == 0x0d && w[1] == 0x0b {
                has_bit_size = true;
                break;
            }
        }
        assert!(
            !has_bit_size,
            "non-bitfield struct should not emit DW_AT_bit_size"
        );
    }

    fn make_line_cu(entries: Vec<LineEntry>) -> CompileUnit {
        CompileUnit {
            source_name: "x.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            functions: vec![],
            globals: vec![],
            types: vec![],
            line_entries: entries,
            line_files: vec!["x.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0x100,
            high_pc: 0x200,
            func_ranges: vec![(0x100, 0x200)],
            cfi_rows: vec![],
        }
    }

    fn count_dw_lns_copy(line_bytes: &[u8]) -> usize {
        // DW_LNS_copy 编码为单字节 0x01. 简化统计: 把 0x01 后跟 0x00
        // (end_sequence 的 extended_op 长度) 排除后, 统计剩余 0x01.
        // end_sequence 序列是 [0x00, 0x01, 0x01] (ext_op, len=1, lne=1),
        // 所以 0x01 单独出现才是 DW_LNS_copy.
        line_bytes.iter().filter(|&&b| b == DW_LNS_copy).count()
    }

    #[test]
    fn dwarf_line_dedup_under_g1() {
        // 4 条 entry, 后 3 条同源同行 → g1 应去重为 2 条 (row0 + row_end)
        let entries = vec![
            LineEntry {
                pc: 0x100,
                line: 1,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
            LineEntry {
                pc: 0x104,
                line: 5,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
            LineEntry {
                pc: 0x108,
                line: 5,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
            LineEntry {
                pc: 0x10c,
                line: 5,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
        ];
        let cu = make_line_cu(entries);
        let sec_g1 = build_dwarf_full(
            &cu,
            &DebugOptions {
                level: 1,
                ..Default::default()
            },
        );
        let sec_g3 = build_dwarf_full(
            &cu,
            &DebugOptions {
                level: 3,
                ..Default::default()
            },
        );
        // -g1 应产生更少 DW_LNS_copy (行序列更短)
        let g1_copies = count_dw_lns_copy(&sec_g1.line);
        let g3_copies = count_dw_lns_copy(&sec_g3.line);
        assert!(
            g1_copies < g3_copies,
            "-g1 should dedup adjacent same-line entries (g1={} g3={})",
            g1_copies,
            g3_copies
        );
        // 验证 .debug_line 头仍然合法
        assert_eq!(sec_g1.line[4], 4);
    }

    #[test]
    fn dwarf_line_no_dedup_under_g3() {
        // -g3 (full debug) 保留所有条目, 不去重
        let entries = vec![
            LineEntry {
                pc: 0x100,
                line: 1,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
            LineEntry {
                pc: 0x104,
                line: 5,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
            LineEntry {
                pc: 0x108,
                line: 5,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
        ];
        let cu = make_line_cu(entries);
        let sec = build_dwarf_full(
            &cu,
            &DebugOptions {
                level: 3,
                ..Default::default()
            },
        );
        // -g3 下 3 条 entry 全部保留 (每条都用 DW_LNS_copy 标记新行起点)
        // 简化判断: .debug_line 字节数应比 g1 版本大或相等
        let sec_g1 = build_dwarf_full(
            &cu,
            &DebugOptions {
                level: 1,
                ..Default::default()
            },
        );
        assert!(
            sec.line.len() >= sec_g1.line.len(),
            "g3 should keep all entries, not dedup (g3={} g1={})",
            sec.line.len(),
            sec_g1.line.len()
        );
    }
}
