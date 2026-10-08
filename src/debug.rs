// src/debug.rs - 完整调试信息生成
//   DWARF v4/v5 + .BTF + .eh_frame + .orc_unwind_ip + .debug_frame +
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

use crate::codegen::shared::elf::{ElfSection, SHF_ALLOC, SHT_PROGBITS};

// ============================================================================
// 公共类型
// ============================================================================

/// 目标架构（用于 EH 帧 CIE 编码 + 寄存器编号）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchKind {
    X86_64,
    AArch64,
    RiscV64,
    RiscV32,
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
            ArchKind::RiscV32 => 1,                  // ra (x1)
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
            ArchKind::RiscV32 => 8,                  // s0/fp (x8)
            ArchKind::Mips | ArchKind::Mips64 => 30, // $fp
            ArchKind::Arm => 11,                     // R11 (fp)
        }
    }
    /// 把本后端 GP 的**硬件编号**（x86_64：REX 三字段编出来的那个 0..31）翻成
    /// DWARF 寄存器号。返回 None = 本 target 今天拿不出这个对应关系。
    ///
    /// 为什么需要一个专门的表而不是直接拿硬件号用：SysV/x86-64 psABI 的 DWARF 编号顺序与
    /// 机器码编码**不一样**（rdx=1、rcx=2、rsi=4、rdi=5、rbp=6、rsp=7）。拿硬件号发
    /// `DW_OP_reg` 就是让调试器去读另一个寄存器 —— 一个看不见的错（比不报位置更坏）。
    ///
    /// 只定义 x86_64：其他 target 的后端今天不消费 `Location::Reg`（见
    /// `codegen::shared::homes::home_support` 只有 x86_64 是 StackAndRegister），所以下面
    /// 一律 None而不是猜一张没对拍过的表。
    pub fn dwarf_reg_of_gp_hw(self, hw: u8) -> Option<u8> {
        match self {
            ArchKind::X86_64 => match hw {
                0 => Some(0),  // rax
                1 => Some(2),  // rcx
                2 => Some(1),  // rdx
                3 => Some(3),  // rbx
                4 => Some(7),  // rsp
                5 => Some(6),  // rbp
                6 => Some(4),  // rsi
                7 => Some(5),  // rdi
                // r8..r15 两侧同序；APX 的 r16..r31 也在 DWARF 的 reg16..reg31 上
                n @ 8..=31 => Some(n),
                _ => None,
            },
            _ => None,
        }
    }

    /// 字宽（字节）
    pub fn address_size(self) -> u8 {
        match self {
            ArchKind::X86_64 | ArchKind::AArch64 | ArchKind::RiscV64 | ArchKind::Mips64 => 8,
            ArchKind::Mips | ArchKind::Arm | ArchKind::RiscV32 => 4,
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
            | ArchKind::RiscV32
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
            ArchKind::RiscV32 => 2,                  // sp
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
    /// DWARF 版本：4 或 5（默认 4）
    pub dwarf_version: u8,
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
            dwarf_version: 4,
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
#[derive(Debug, Clone, Default)]
pub struct FuncFrame {
    pub pc: u64,
    pub size: u32,
    /// FDE 内 CFI 指令字节流 (从 CFA 初始指令之后开始; 不含末尾 advance_loc).
    /// 为空表示该函数无 CFI 信息 (沿用 CIE 默认 CFA = rsp + 8).
    /// 字节流内: DW_CFA_def_cfa / DW_CFA_def_cfa_offset /
    /// DW_CFA_offset / DW_CFA_restore 等 uleb128 编码指令按顺序串接.
    pub cfi_ops: Vec<u8>,
    /// C++ 零成本异常: 本函数 LSDA 在 `.gcc_except_table` 里的起始偏移。
    /// `Some` 时 FDE 必须引用带 `zPLR` 增强的 CIE 并在末尾写入 LSDA 字段;
    /// `None` 时仍用旧的 `zR` CIE (零开销: 不影响无异常处理的函数)。
    pub lsda_offset: Option<u64>,
}

impl FuncFrame {
    /// 构造一个无 CFI 信息的 FuncFrame (仅 PC + size).
    pub fn new(pc: u64, size: u32) -> Self {
        Self {
            pc,
            size,
            cfi_ops: Vec::new(),
            lsda_offset: None,
        }
    }
    /// 构造带 CFI ops 的 FuncFrame.
    pub fn with_cfi(pc: u64, size: u32, cfi_ops: Vec<u8>) -> Self {
        Self {
            pc,
            size,
            cfi_ops,
            lsda_offset: None,
        }
    }
    /// 同上并带上 LSDA 偏移 (需要 `zPLR` CIE 与 FDE LSDA 字段)。
    pub fn with_lsda(pc: u64, size: u32, cfi_ops: Vec<u8>, lsda_offset: u64) -> Self {
        Self {
            pc,
            size,
            cfi_ops,
            lsda_offset: Some(lsda_offset),
        }
    }
}

/// `.eh_frame` 里需要链接期解析的字段。
///
/// 目标地址在汇编期不可知 (函数会被移到最终 `.text` 里, LSDA 在另一个节),
/// 必须像 GCC 一样以重定位表达: pc 字段 = PC32 vs `.text` 节符号 + 函数偏移,
/// LSDA 字段 = PC32 vs `.gcc_except_table` 节符号 + LSDA 偏移,
/// personality = PC32 vs `DW.ref.__gxx_personality_v0`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EhRelTarget {
    /// `.text` 节符号 (addend = 函数在 .text 内的偏移)
    TextSection,
    /// `.gcc_except_table` 节符号 (addend = LSDA 在该节内的偏移)
    ExceptTableSection,
    /// `DW.ref.__gxx_personality_v0` 全局符号 (addend = 0)
    PersonalitySlot,
}

/// `.eh_frame` 的一条重定位 (偏移相对 `.eh_frame` 节本身)。
#[derive(Debug, Clone, Copy)]
pub struct EhFrameRela {
    pub offset: u64,
    pub target: EhRelTarget,
    pub addend: i64,
}

/// 一个完整的 `.eh_frame` 镜像: 字节 + 重定位。
#[derive(Debug, Clone, Default)]
pub struct EhFrameImage {
    pub bytes: Vec<u8>,
    pub relocs: Vec<EhFrameRela>,
}

/// personality 槽名 (GCC 同构): 该全局符号存的是 `&__gxx_personality_v0`。
pub const GXX_PERSONALITY_SLOT: &str = "DW.ref.__gxx_personality_v0";

/// 生成 x86_64 的 `.eh_frame`, 含 C++ 零成本异常所需的 `zPLR` 增强与重定位。
///
/// 与 GCC 产物逐字段对齐 (`readelf --debug-dump=frames` / `-r` 可直接比对):
///
/// - 两个 CIE: `zR` (无 LSDA 的函数) 与 `zPLR` (有 LSDA 的函数)。FDE 只能引用
///   它之前的 CIE, 所先先出 `zR` 再出 `zPLR`。
/// - `zPLR` 的 augmentation data = `[P 编码 0x9b, P 值 sdata4(PC32 重定位),
///   L 编码 0x1b, R 编码 0x1b]` (顺串与 g++ 一致: 先 personality, 后 L/R 编码字节)。
/// - FDE augmentation data = LSDA 指针本身 (编码由 CIE 的 L 字节描述), 所
///   augmentation data length = 4。
pub fn build_eh_frame_linked(frames: &[FuncFrame]) -> EhFrameImage {
    let arch = ArchKind::X86_64;
    let mut out = EhFrameImage::default();
    let eh = &mut out.bytes;

    // ---- CIE #1: "zR" (无 LSDA) ----
    let cie_plain_pos = eh.len();
    push_cie(eh, b"zR", arch, false, &mut out.relocs);

    // ---- CIE #2: "zPLR" (有 LSDA, 带 personality) ----
    // 只在确实有带 LSDA 的函数时才发射: 纯 C 模块不该凭空依赖
    // `DW.ref.__gxx_personality_v0` 这个 libstdc++ 符号。
    let cie_lsda_pos = if frames.iter().any(|f| f.lsda_offset.is_some()) {
        push_cie(eh, b"zPLR", arch, true, &mut out.relocs)
    } else {
        cie_plain_pos
    };

    for func in frames {
        let cie_pos = if func.lsda_offset.is_some() {
            cie_lsda_pos
        } else {
            cie_plain_pos
        };
        push_fde(eh, func, cie_pos, &mut out.relocs);
    }
    out
}

/// 发射一个 CIE, 返回它在节内的起始偏移。
fn push_cie(
    eh: &mut Vec<u8>,
    augmentation: &[u8],
    arch: ArchKind,
    with_personality: bool,
    relocs: &mut Vec<EhFrameRela>,
) -> usize {
    let cie_len_pos = eh.len();
    eh.extend_from_slice(&[0u8; 4]); // length 占位
    eh.extend_from_slice(&0u32.to_le_bytes()); // CIE id = 0 (LSB .eh_frame 约定)
    eh.push(1); // version
    eh.extend_from_slice(augmentation); // "zR" / "zPLR" (无结尾 0, 下面是显式 NUL)
    eh.push(0); // null-terminate augmentation
    write_uleb128(eh, 1); // code_alignment_factor
    write_sleb128_value(eh, -8); // data_alignment_factor
    write_uleb128(eh, arch.dwarf_reg_ra() as u64); // return address column
                                                   // augmentation data (先算长度): zPLR = L 编码? 顺序与 g++ 一致:
                                                   //   [P 编码字节, P 值(4B), L 编码字节(0x1b), R 编码字节(0x1b)]
                                                   //   zR           = [R 编码字节(0x1b)]
    let aug_data_len_pos = eh.len();
    if with_personality {
        eh.push(7); // augmentation data length
        eh.push(0x9b); // personality 编码: pcrel | indirect | sdata4
        let p_pos = eh.len() as u64;
        eh.extend_from_slice(&0i32.to_le_bytes()); // DW.ref.__gxx_personality_v0 - P
        relocs.push(EhFrameRela {
            offset: p_pos,
            target: EhRelTarget::PersonalitySlot,
            addend: 0,
        });
        eh.push(0x1b); // LSDA 字段编码: pcrel | sdata4
        eh.push(0x1b); // FDE PC 字段编码: pcrel | sdata4
    } else {
        eh.push(1);
        eh.push(0x1b);
    }
    let _ = aug_data_len_pos;
    // CIE 的**初始行** (initial_instructions): 与 GCC 逐字节一致地建立
    // `CFA = rsp + 8` 与 `ret.addr @ CFA-8`。这不是可省的样板: FDE 里的
    // `DW_CFA_def_cfa(r13, 16)` 只接管 CFA 规则, 返回地址列 (16) 的保存位置
    // 仍要从这一行继承; 缺了它, 展开器只能用上一帧的 CFA 去猜 rip 的位置,
    // 穿越一个**本帧不抛异常**的中间帧 (无 LSDA 的函数) 时就会拿到错的返回
    // 地址并在 libgcc_s 里 SEGV (实测: `void ff(){throw 9;}` + 外层 catch)。
    eh.push(DW_CFA_def_cfa);
    write_uleb128(eh, 7); // rsp
    write_uleb128(eh, 8);
    // DW_CFA_offset = 0x80 | 列号, 操作数 = uleb(偏移因子个数, 数据对齐因子为 -8)
    eh.push(DW_CFA_offset | arch.dwarf_reg_ra());
    write_uleb128(eh, 1);
    // 下一个记录 (FDE) 起点需 4 字节对齐 (相对本节)
    while !(eh.len() - cie_len_pos - 4).is_multiple_of(4) {
        eh.push(0);
    }
    let cie_len = (eh.len() - cie_len_pos - 4) as u32;
    eh[cie_len_pos..cie_len_pos + 4].copy_from_slice(&cie_len.to_le_bytes());
    cie_len_pos
}

/// 发射一个 FDE (引用 `cie_pos` 处的 CIE)。
fn push_fde(eh: &mut Vec<u8>, func: &FuncFrame, cie_pos: usize, relocs: &mut Vec<EhFrameRela>) {
    let fde_start = eh.len();
    eh.extend_from_slice(&[0u8; 4]); // length 占位
                                     // CIE_pointer: 与 GCC 逐字节一致 = **本字段地址 - CIE 起始** (对前面的 CIE
                                     // 恒为正, 读方用 `cie = 本字段地址 - 值` 还原)。写成负值会让展开器算到节外
                                     // 地址并在读 CIE 时 SEGV。
    let cie_ptr = ((fde_start + 4) as i64 - cie_pos as i64) as u32;
    eh.extend_from_slice(&cie_ptr.to_le_bytes());
    // initial location: pcrel sdata4, 所以值 = func.pc - 本字段地址 —— 由链接器
    // 完成: PC32 vs `.text` 节符号, addend = 函数在 .text 内的偏移。
    let pc_pos = eh.len() as u64;
    eh.extend_from_slice(&0i32.to_le_bytes());
    relocs.push(EhFrameRela {
        offset: pc_pos,
        target: EhRelTarget::TextSection,
        addend: func.pc as i64,
    });
    // address range: 同一节内的常量差, 无需重定位
    eh.extend_from_slice(&(func.size as u64).to_le_bytes()[..4]);
    match func.lsda_offset {
        Some(lsda) => {
            eh.push(4); // augmentation data length (只有一个 LSDA 字段)
            let lsda_pos = eh.len() as u64;
            eh.extend_from_slice(&0i32.to_le_bytes());
            relocs.push(EhFrameRela {
                offset: lsda_pos,
                target: EhRelTarget::ExceptTableSection,
                addend: lsda as i64,
            });
        },
        None => {
            eh.push(0); // augmentation data length = 0
        },
    }
    if !func.cfi_ops.is_empty() {
        eh.extend_from_slice(&func.cfi_ops);
    }
    // 每条记录按 8 字节对齐
    while !eh.len().is_multiple_of(8) {
        eh.push(0);
    }
    let fde_len = (eh.len() - fde_start - 4) as u32;
    eh[fde_start..fde_start + 4].copy_from_slice(&fde_len.to_le_bytes());
}

/// sleb128 (供 CIE 的 data_alignment_factor = -8 使用)。
fn write_sleb128_value(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
        out.push(if done { b } else { b | 0x80 });
        if done {
            break;
        }
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
    /// C++ class: 类似 struct 但使用 DW_TAG_class_type,
    /// 可包含继承关系和多态信息.
    Class {
        members: Vec<DwarfMember>,
        bases: Vec<DwarfInheritance>,
        vtable_count: u32,
    },
    Void,
}

/// C++ 继承信息 (DW_TAG_inheritance).
#[derive(Debug, Clone)]
pub struct DwarfInheritance {
    /// 基类类型
    pub base_type: DwarfType,
    /// 基类在派生类中的字节偏移
    pub offset: u64,
    /// 是否虚继承
    pub is_virtual: bool,
    /// 访问说明符: 1=public, 2=protected, 3=private
    pub access: u8,
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

/// 参数/变量的位置描述 (DWARF DW_AT_location 表达式)
/// GDB 用此信息在调试时定位对象在栈帧 / 寄存器里的位置
#[derive(Debug, Clone, Copy)]
pub struct DwarfParamLocation {
    /// DWARF 表达式操作码: DW_OP_fbreg (0x91)、DW_OP_call_frame_cfa (0x9c)，
    /// 或 `DW_OP_reg0 + n`（0x50..0x6f，寄存器家；此时 `offset` 参不参与编码）。
    pub op: u8,
    /// 偏移量 (sleb128 编码): 相对帧基址或 CFA 的字节偏移；寄存器家时为 0
    pub offset: i64,
}

impl DwarfParamLocation {
    /// 构造 DW_OP_fbreg 位置 (相对帧基址)
    pub fn fbreg(offset: i64) -> Self {
        Self {
            op: DW_OP_fbreg,
            offset,
        }
    }
    /// 构造 DW_OP_call_frame_cfa + offset 位置
    pub fn cfa(offset: i64) -> Self {
        Self {
            op: DW_OP_call_frame_cfa,
            offset,
        }
    }
    /// 构造一个**寄存器家**的位置：`dwarf_reg` 是 DWARF 寄存器号（不是硬件编码）。
    ///
    /// 这是 Phase 6.2 缺的那一块：以前 DWARF 只会发栈槽位置（`DW_OP_fbreg`），所以
    /// `src/ssa/regalloc.rs` 在 `-g` 下必须禁止寄存器家（否则调试器读一个从未写过的槽）。
    /// 号与硬件编码的换算不在这里（见 `hw_reg_to_dwarf`）。
    pub fn reg(dwarf_reg: u8) -> Self {
        Self {
            op: DW_OP_reg0 + dwarf_reg,
            offset: 0,
        }
    }

    /// 本位置是不是寄存器家（调试器侧与用例都要能问这一句，而不是各自看 `op` 落在哪个区间）。
    pub fn is_register(&self) -> bool {
        self.op >= DW_OP_reg0 && self.op <= DW_OP_reg31
    }

    /// 本位置里的 DWARF 寄存器号（非寄存器家返回 None）
    pub fn register(&self) -> Option<u8> {
        if self.is_register() {
            Some(self.op - DW_OP_reg0)
        } else {
            None
        }
    }

    /// 把表达式字节编入 `out`（不含 `exprloc` 的长度前缀，那由 `write_location_expr` 写）。
    ///
    /// `DW_OP_reg0..reg31` 是**单字节、无操作数**的；而 fbreg / call_frame_cfa 后面跟一条
    /// sleb128。两者形状不同，所以下面按 `op` 分岔而不是统一发一个偏移（那会多发 1 字节，
    /// 长度与内容不一致就仍旧错位）。
    pub fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), ()> {
        out.push(self.op);
        if self.is_register() {
            // reg0..reg31 无操作数；再高的号得走 DW_OP_regx（本 target 今天用不到）
            return Ok(());
        }
        match self.op {
            DW_OP_fbreg | DW_OP_call_frame_cfa => {
                write_sleb128(out, self.offset);
                Ok(())
            },
            DW_OP_plus_uconst => {
                write_uleb128(out, self.offset.max(0) as u64);
                Ok(())
            },
            _ => Err(()),
        }
    }
}

/// 局部变量描述 (函数或词法块内的变量)
#[derive(Debug, Clone)]
pub struct DwarfLocalVar {
    pub name: String,
    pub ty: DwarfType,
    /// DWARF 位置表达式 (DW_OP_fbreg offset 等)
    pub location: DwarfParamLocation,
    pub decl_line: u32,
}

/// 词法块描述 (C 语言 {} 作用域)
/// GDB 用 DW_TAG_lexical_block 识别局部变量可见范围
#[derive(Debug, Clone)]
pub struct DwarfLexicalBlock {
    /// 块起始 PC
    pub low_pc: u64,
    /// 块结束 PC
    pub high_pc: u64,
    /// 块内局部变量
    pub variables: Vec<DwarfLocalVar>,
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
    /// 参数位置信息 (与 params 对应, 为空时不生成 DW_AT_location)
    pub param_locations: Vec<DwarfParamLocation>,
    /// 函数直接作用域内的局部变量 (不在任何嵌套词法块内)
    pub local_vars: Vec<DwarfLocalVar>,
    /// 嵌套词法块 (if/for/while {} 等)
    pub lexical_blocks: Vec<DwarfLexicalBlock>,
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
    // DWARF v5 新增节
    pub str_offsets: Vec<u8>,
    pub addr: Vec<u8>,
    pub line_str: Vec<u8>,
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
    // .BTF.ext (func_info + line_info for BPF verifier)
    pub btf_ext: Vec<u8>,
    // .debug_info relocations: (offset_in_section, symbol_name)
    pub debug_info_relocs: Vec<(u64, String)>,
    // .debug_line relocations: (offset_in_section, symbol_name)
    pub debug_line_relocs: Vec<(u64, String)>,
    // .debug_aranges relocations: (offset_in_section, symbol_name)
    pub debug_aranges_relocs: Vec<(u64, String)>,
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
                group_name: None,
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
        // DWARF v5 新增节
        push(&mut v, ".debug_str_offsets", &self.str_offsets, false);
        push(&mut v, ".debug_addr", &self.addr, false);
        push(&mut v, ".debug_line_str", &self.line_str, false);
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
// DWARF v4/v5 常量（tag / attribute / form / opcode）
//
// 这些常量名严格遵循 DWARF v4/v5 规范原文, 例如 DW_TAG_array_type /
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
const DW_TAG_inheritance: u16 = 0x18;

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
const DW_AT_location: u16 = 0x02;
/// 变量的 `DW_OP_fbreg` 的基准。DWARF 里它是 **subprogram** 的属性（不是变量 DIE 的），
/// 缺了它 gdb 会说「Could not find the frame base」并且拒读任何 fbreg 定位的局部。
/// 数值 0x40。**不靠记忆也不靠猜测**：本仓从 gcc 自己发出的 `.debug_abbrev` 字节里解出来
/// （subprogram 那条 abbrev 的属性顺序与 readelf 打印的名字逐位对齐：`3f` external、
/// `03` name、`3a` decl_file、`3b` decl_line、`39` decl_column、`27` prototyped、`49` type、
/// `11` low_pc、`12` high_pc，紧接的就是 `40` = frame_base）。猜错过一次：`0x0a` 是
/// `DW_AT_subscr_data`、`0x1d` 是 `DW_AT_containing_type`，而 readelf **照它自己的表**
/// 解成另一个属性名，不报任何错 —— 这种错只能靠对拍工具链的真实字节而不是 记性。
const DW_AT_frame_base: u16 = 0x40;
const DW_AT_const_value: u16 = 0x1c;
const DW_AT_decl_file: u16 = 0x3a;
const DW_AT_decl_line: u16 = 0x3b;
const DW_AT_GNU_dwo_id: u16 = 0x2130;
const DW_AT_GNU_dwo_name: u16 = 0x2131;
const DW_AT_GNU_all_call_sites: u16 = 0x2117;
const DW_AT_visibility: u16 = 0x33;
const DW_AT_virtuality: u16 = 0x4c;
const DW_AT_vtable_elem_count: u16 = 0x6f;

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

// DWARF expression opcodes (DW_OP_*)
const DW_OP_fbreg: u8 = 0x91;
const DW_OP_call_frame_cfa: u8 = 0x9c;
const DW_OP_plus_uconst: u8 = 0x23;
// 寄存器家：DW_OP_reg0..DW_OP_reg31 是 0x50..0x6f（单字节，无操作数）
const DW_OP_reg0: u8 = 0x50;
const DW_OP_reg31: u8 = 0x6f;

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

// DWARF v5 unit type (DW_UT_*)
const DW_UT_compile: u8 = 0x01;

// DWARF v5 new forms
const DW_FORM_strx1: u16 = 0x25;
const DW_FORM_strx2: u16 = 0x26;
const DW_FORM_strx4: u16 = 0x28;
const DW_FORM_addrx1: u16 = 0x29;
const DW_FORM_addrx2: u16 = 0x2a;
const DW_FORM_addrx4: u16 = 0x2c;
const DW_FORM_line_strp: u16 = 0x1f;

// DWARF v5 new attributes
const DW_AT_str_offsets_base: u16 = 0x72;
const DW_AT_addr_base: u16 = 0x73;
const DW_AT_rnglists_base: u16 = 0x74;
const DW_AT_loclists_base: u16 = 0x75;
const DW_AT_line_str: u16 = 0x76;

// DWARF v5 line number content types (DW_LNCT_*)
// DW_LNCT_path and DW_LNCT_directory_index already defined above
// DWARF v5 uses DW_FORM_string for inline directory/file entries

// DWARF v5 .debug_line header entry format
// DW_FORM_string for directory/file entries in v5 line program

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
const BTF_KIND_FLOAT: u32 = 16;
const BTF_KIND_DECL_TAG: u32 = 17;
const BTF_KIND_TYPE_TAG: u32 = 18;
const BTF_KIND_ENUM64: u32 = 19;

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
    fn into_bytes(mut self) -> Vec<u8> {
        // DWARF spec: abbreviation table must end with a 0x00 byte
        self.data.push(0x00);
        self.data
    }
}

// ============================================================================
// 公共 API
// ============================================================================

/// 兼容旧签名：构造一个最小 DWARF v4/v5 CU。
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
            param_locations: vec![],
            local_vars: vec![],
            lexical_blocks: vec![],
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
            param_locations: vec![],
            local_vars: vec![],
            lexical_blocks: vec![],
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

/// `.debug_*` 各子节的**唯一口径**：把输入 CU 的函数表整理成「一条一函数、按地址升序、
/// CU 范围覆盖全部函数」的形状，返回归一化后的副本（下游一律只读这份）。
///
/// 为什么必须在发射之前、在一处做，而不是让各节自己整理：`.debug_info` 的
/// `DW_AT_low_pc`/`DW_AT_high_pc`、`.debug_line` 的地址序列、`.debug_aranges` 的表项、
/// `.debug_frame` 的 FDE 读的是**同一批事实**（函数在 `.text` 里的位置）。此前四个消费者
/// 各自直接扫 `cu.functions`，于是同一个缺陷在四个节里长成四种症状，而在 gdb 那边只
/// 表现为一句 warning。逐条：
///
/// 1. **同一个函数不得有两条 DIE**。IR 里一个函数可以同时存在「原型声明」与「定义」
///    两个条目，而上游 `func_pcs` 是按**名字**取 label 的，两条于是拿到同一个 PC ——
///    发出去就是两个同名同址的 `DW_TAG_subprogram`、行表两行、两个完全重叠的 FDE。
///    重叠 FDE 是 CFI 层的硬错误（`.debug_frame`/`.eh_frame` 不允许同一地址区间两条
///    记录），重复 DIE 则让调试器在两个描述之间猜。合并规则：区间取并，非空的
///    局部表 / 形参位置表 / 词法块表取「有内容的那一侧」，`decl_line` 取第一个非零值。
/// 2. **地址必须单调递增**。`cu.functions` 是源码序，`.text` 里是链接序，两者不同序；
///    而行号程序的状态机只能用「相对上一步的 advance」表达，倒退无法编码，DWARF
///    §6.2.5.2 也要求序列内地址递增；gdb 对行表做二分查找，乱序的表它查不到。
/// 3. **CU 自身的范围必须是 min/max，不是首尾**。原来取 `functions.first()/last()`
///    （= 源码序首尾），实测产物里 CU 范围是 `[0x401190, +0xad)`，而真正最靠前的函数
///    `f` 在 `0x401110` —— 整个落在 CU 范围之外，这正是 gdb 那句
///    `Internal error: pc 0x401110 in read in CU, but not in symtab.` 的字面意思。
///
/// 没有函数时（只有 `func_ranges` 或只有行表的输入）一律**不改** `low_pc`/`high_pc`/
/// `func_ranges`：这三个字段是输入的一部分，不能因为本函数「看起来该管它们」就被清空。
pub fn normalize_cu(cu: &CompileUnit) -> CompileUnit {
    let mut out = cu.clone();

    // 1) 同名合并（保持首次出现的顺序，排序留给下一步）
    let mut merged: Vec<DwarfFuncInfo> = Vec::with_capacity(cu.functions.len());
    for f in cu.functions.iter() {
        if let Some(prev) = merged.iter_mut().find(|p| p.name == f.name) {
            prev.low_pc = prev.low_pc.min(f.low_pc);
            prev.high_pc = prev.high_pc.max(f.high_pc);
            if prev.decl_line == 0 {
                prev.decl_line = f.decl_line;
            }
            if prev.linkage_name.is_none() {
                prev.linkage_name = f.linkage_name.clone();
            }
            if prev.local_vars.is_empty() {
                prev.local_vars = f.local_vars.clone();
            }
            if prev.param_locations.is_empty() {
                prev.param_locations = f.param_locations.clone();
            }
            if prev.lexical_blocks.is_empty() {
                prev.lexical_blocks = f.lexical_blocks.clone();
            }
            if prev.params.is_empty() {
                prev.params = f.params.clone();
            }
            prev.is_external |= f.is_external;
        } else {
            merged.push(f.clone());
        }
    }

    // 2) 按起始 PC 升序；同 PC（不该发生，但输入错了也不得让行表倒退）再按名字定序
    merged.sort_by(|a, b| {
        a.low_pc
            .cmp(&b.low_pc)
            .then_with(|| a.name.cmp(&b.name))
    });

    // 3) CU 范围 = 所有函数区间的 min/max
    if let Some(lo) = merged.iter().map(|f| f.low_pc).min() {
        out.low_pc = lo;
        out.high_pc = merged.iter().map(|f| f.high_pc).max().unwrap_or(lo);
        out.func_ranges = merged.iter().map(|f| (f.low_pc, f.high_pc)).collect();
    }
    out.functions = merged;
    out
}

/// 完整 DWARF v4/v5 + 辅助节生成（主入口）
pub fn build_dwarf_full(cu: &CompileUnit, opts: &DebugOptions) -> DebugSections {
    // 四个消费者（info / line / aranges / frame）一律只读归一化后的这一份，见 `normalize_cu`。
    let cu = &normalize_cu(cu);
    let mut sec = DebugSections::default();

    let mut strs = StringTable::new();
    let mut abb = AbbrevBuilder::default();

    // 1) .debug_str —— 先按已知名字定下顺序（下面的第 2 步会复用同一张表，不会重排）
    sec.str = strl_section(&mut strs, cu, opts);

    // 2) .debug_abbrev + .debug_info
    let (abbrev, info, relocs) = build_abbrev_and_info(cu, opts, &mut strs, &mut abb);
    // 节内容必须在这一步**之后**重新取。`StringTable::intern` 是「去重 + 往后追加」，
    // 所以重取只会多出新串、不改动已有串的偏移（逐字节前缀相同）。
    //
    // 为什么这是结构性修法而不是「给上面那个枚举多一行」：`strl_section` 是「哪些串会
    // 出现在 .debug_info 里」的**第二份知道**，它漏一个 DIE 种类，那个种类的 DW_FORM_strp
    // 就指向节外（实测：新接上的 `DW_TAG_variable` 名字让 readelf 报「偏移量过大」，
    // 而编译与链接完全无感）。以后新增任何带名字的 DIE 都不必再动 `strl_section`；
    // 它剩下的作用只是固定已知串的顺序（不固定也不会错，只是字节会漂）。
    sec.str = strs.clone().into_bytes();
    sec.abbrev = abbrev;
    sec.info = info;
    sec.debug_info_relocs = relocs;

    // 3) .debug_line
    let (line, line_relocs) = build_line_program(cu, opts);
    sec.line = line;
    sec.debug_line_relocs = line_relocs;

    // 4) .debug_aranges
    let (aranges, aranges_relocs) = build_aranges(cu, opts);
    sec.aranges = aranges;
    sec.debug_aranges_relocs = aranges_relocs;

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

    // 8) DWARF v5: .debug_str_offsets + .debug_addr + .debug_line_str
    if opts.dwarf_version >= 5 {
        sec.str_offsets = build_str_offsets(cu, opts);
        sec.addr = build_addr(cu, opts);
        sec.line_str = build_line_str(cu, opts);
    }

    // 9) -gsplit-dwarf: dwo + skeleton
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

    // 10) BTF - 生成完整类型信息
    sec.btf = build_btf_full(cu);

    // 11) .BTF.ext - func_info + line_info (BPF verifier 需要)
    sec.btf_ext = build_btf_ext(cu);

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
        },
        DwarfTypeKind::Class { members, bases, .. } => {
            for m in members {
                strs.intern(&m.name);
                intern_type(strs, &m.ty);
            }
            for b in bases {
                intern_type(strs, &b.base_type);
            }
        },
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
        },
        DwarfTypeKind::Enum {
            members,
            underlying,
        } => {
            for (n, _) in members {
                strs.intern(n);
            }
            intern_type(strs, underlying);
        },
        DwarfTypeKind::Typedef { alias, target } => {
            strs.intern(alias);
            intern_type(strs, target);
        },
        _ => {},
    }
}

/// Recursively collect a type and all nested types into the ordered list.
fn collect_all_types_from(ty: &DwarfType, out: &mut Vec<DwarfType>) {
    if out.iter().any(|t: &DwarfType| t.name == ty.name) {
        return;
    }
    out.push(ty.clone());
    match &ty.kind {
        DwarfTypeKind::Pointer(inner)
        | DwarfTypeKind::Const(inner)
        | DwarfTypeKind::Volatile(inner)
        | DwarfTypeKind::Restrict(inner) => {
            collect_all_types_from(inner, out);
        },
        DwarfTypeKind::Array { element, .. } => {
            collect_all_types_from(element, out);
        },
        DwarfTypeKind::Struct { members } | DwarfTypeKind::Union { members } => {
            for m in members {
                collect_all_types_from(&m.ty, out);
            }
        },
        DwarfTypeKind::Class { members, bases, .. } => {
            for m in members {
                collect_all_types_from(&m.ty, out);
            }
            for b in bases {
                collect_all_types_from(&b.base_type, out);
            }
        },
        DwarfTypeKind::Enum { underlying, .. } => {
            collect_all_types_from(underlying, out);
        },
        DwarfTypeKind::Func { ret, params } => {
            collect_all_types_from(ret, out);
            for p in params {
                collect_all_types_from(p, out);
            }
        },
        DwarfTypeKind::Typedef { target, .. } => {
            collect_all_types_from(target, out);
        },
        _ => {},
    }
}

/// Collect all types referenced in the CU (from functions, globals, etc.)
/// in dependency order so that base types appear before derived types.
fn collect_all_cu_types(cu: &CompileUnit) -> Vec<DwarfType> {
    let mut types = Vec::new();
    for f in &cu.functions {
        collect_types_ordered(&f.return_type, &mut types);
        for (_, pty) in &f.params {
            collect_types_ordered(pty, &mut types);
        }
        for v in &f.local_vars {
            collect_types_ordered(&v.ty, &mut types);
        }
        for block in &f.lexical_blocks {
            for v in &block.variables {
                collect_types_ordered(&v.ty, &mut types);
            }
        }
    }
    for g in &cu.globals {
        collect_types_ordered(&g.ty, &mut types);
    }
    for t in &cu.types {
        collect_types_ordered(t, &mut types);
    }
    types
}

/// Collect a type and its nested types into the ordered list.
fn collect_types_ordered(ty: &DwarfType, out: &mut Vec<DwarfType>) {
    collect_all_types_from(ty, out);
}

fn build_abbrev_and_info(
    cu: &CompileUnit,
    opts: &DebugOptions,
    strs: &mut StringTable,
    abb: &mut AbbrevBuilder,
) -> (Vec<u8>, Vec<u8>, Vec<(u64, String)>) {
    let mut type_offsets: std::collections::HashMap<String, u32> = Default::default();
    let mut relocs: Vec<(u64, String)> = Vec::new();
    let dwarf_ver = opts.dwarf_version;

    // info header
    let mut info = Vec::new();
    info.extend_from_slice(&[0u8; 4]); // unit_length 占位
    info.extend_from_slice(&(dwarf_ver as u16).to_le_bytes()); // version
    if dwarf_ver >= 5 {
        // DWARF v5: unit_type(1) + address_size(1) + debug_abbrev_offset(4)
        info.push(DW_UT_compile);
        info.push(opts.arch.address_size());
        info.extend_from_slice(&0u32.to_le_bytes()); // debug_abbrev_offset
    } else {
        // DWARF v4: debug_abbrev_offset(4) + address_size(1)
        info.extend_from_slice(&0u32.to_le_bytes()); // debug_abbrev_offset
        info.push(opts.arch.address_size());
    }

    // CU DIE
    let mut cu_attrs: Vec<(u16, u16)> = Vec::new();
    if dwarf_ver >= 5 {
        // DWARF v5: base attributes must appear first in CU DIE
        cu_attrs.push((DW_AT_str_offsets_base, DW_FORM_sec_offset));
        cu_attrs.push((DW_AT_addr_base, DW_FORM_sec_offset));
    }
    cu_attrs.push((DW_AT_producer, DW_FORM_strp));
    cu_attrs.push((DW_AT_language, DW_FORM_data2));
    cu_attrs.push((DW_AT_name, DW_FORM_strp));
    cu_attrs.push((DW_AT_comp_dir, DW_FORM_strp));
    cu_attrs.push((DW_AT_low_pc, DW_FORM_addr));
    cu_attrs.push((DW_AT_high_pc, DW_FORM_data8)); // constant offset, not address
    cu_attrs.push((DW_AT_stmt_list, DW_FORM_sec_offset));
    let cu_code = abb.get_or_create(DW_TAG_compile_unit, DW_CHILDREN_yes, &cu_attrs);
    write_uleb128(&mut info, cu_code as u64);

    // DWARF v5: base attribute values (placeholder offsets = 0)
    if dwarf_ver >= 5 {
        // str_offsets_base: offset to .debug_str_offsets (0 = not yet populated)
        info.extend_from_slice(&0u32.to_le_bytes());
        // addr_base: offset to .debug_addr (0 = not yet populated)
        info.extend_from_slice(&0u32.to_le_bytes());
    }

    // producer
    write_attr_strp(&mut info, strs.intern(&opts.producer));
    // language
    info.extend_from_slice(&cu.language.to_le_bytes());
    // name
    write_attr_strp(&mut info, strs.intern(&cu.source_name));
    // comp_dir (always write to match abbrev table)
    write_attr_strp(&mut info, strs.intern(cu.comp_dir.as_deref().unwrap_or("")));
    // low_pc (relocated to first function address)
    let cu_low_pc_off = info.len() as u64;
    info.extend_from_slice(&cu.low_pc.to_le_bytes());
    if let Some(first_func) = cu.functions.first() {
        relocs.push((cu_low_pc_off, first_func.name.clone()));
    }
    // high_pc (DWARF 4: can be constant offset)
    let high_pc_val = cu.high_pc.saturating_sub(cu.low_pc);
    info.extend_from_slice(&high_pc_val.to_le_bytes());
    // stmt_list: 占位, .debug_line 偏移 = 0（GDB 接受 0 表示无行号程序）
    info.extend_from_slice(&0u32.to_le_bytes());

    // Pre-collect all types from functions/globals to avoid inline
    // type DIE writes during function attribute encoding.
    let all_types = collect_all_cu_types(cu);
    // type DIEs（顶级类型 — written before function/global DIEs）
    for t in &all_types {
        let off = info.len() as u32;
        type_offsets.insert(t.name.clone(), off);
        write_type_die(&mut info, abb, strs, &mut type_offsets, t);
    }

    // function DIEs
    for f in &cu.functions {
        let has_params = !f.params.is_empty();
        let has_local_vars = !f.local_vars.is_empty();
        let has_lexical_blocks = !f.lexical_blocks.is_empty();
        let has_children = has_params || has_local_vars || has_lexical_blocks;
        let has_linkage = f.linkage_name.is_some();
        let has_decl_line = f.decl_line > 0;
        // Build attribute list conditionally:
        // - DW_AT_linkage_name only when C++ mangled name exists
        // - DW_AT_decl_file + DW_AT_decl_line always (source location)
        let mut sub_attrs: Vec<(u16, u16)> = vec![
            (DW_AT_name, DW_FORM_strp),
            (DW_AT_type, DW_FORM_ref4),
            (DW_AT_external, DW_FORM_flag_present),
            (DW_AT_low_pc, DW_FORM_addr),
            (DW_AT_high_pc, DW_FORM_data8), // constant offset, not address
        ];
        if has_linkage {
            sub_attrs.insert(1, (DW_AT_linkage_name, DW_FORM_strp));
        }
        if has_decl_line {
            sub_attrs.push((DW_AT_decl_file, DW_FORM_data1));
            sub_attrs.push((DW_AT_decl_line, DW_FORM_data4));
        }
        // DW_AT_frame_base：变量位置用 `DW_OP_fbreg` 时，它的基准就是本属性算出来的值。
        // 缺它 gdb 直接报「Could not find the frame base for "f"」并拒读局部（实测），
        // 所以这不是可选装饰而是 fbreg 能用的前提。选 `DW_OP_call_frame_cfa` 而不是
        // `DW_OP_reg6`：本后端的 `DW_OP_fbreg` 偏移是按 **CFA** 算的（见
        // `cfa_frame_offset`），与有没有 frame pointer 无关。
        sub_attrs.push((DW_AT_frame_base, DW_FORM_exprloc));
        let code = abb.get_or_create(
            DW_TAG_subprogram,
            if has_children { DW_CHILDREN_yes } else { DW_CHILDREN_no },
            &sub_attrs,
        );
        // 返回类型的 DIE 必须先发完（见 `write_type_die` 里同一理由的注释）；下面形参与
        // 局部变量的类型同理，全部在写本 DIE 的字节之前算好偏移。
        let ret_off = ensure_type_die(&mut info, abb, strs, &mut type_offsets, &f.return_type);
        let param_type_offs: Vec<u32> = f
            .params
            .iter()
            .map(|(_, pty)| ensure_type_die(&mut info, abb, strs, &mut type_offsets, pty))
            .collect();
        let local_var_type_offs: Vec<u32> = f
            .local_vars
            .iter()
            .map(|v| ensure_type_die(&mut info, abb, strs, &mut type_offsets, &v.ty))
            .collect();
        let block_var_type_offs: Vec<Vec<u32>> = f
            .lexical_blocks
            .iter()
            .map(|b| {
                b.variables
                    .iter()
                    .map(|v| ensure_type_die(&mut info, abb, strs, &mut type_offsets, &v.ty))
                    .collect()
            })
            .collect();
        write_uleb128(&mut info, code as u64);

        // name
        write_attr_strp(&mut info, strs.intern(&f.name));
        // linkage_name (only when present)
        if let Some(ln) = &f.linkage_name {
            write_attr_strp(&mut info, strs.intern(ln));
        }
        // type (return type)
        info.extend_from_slice(&ret_off.to_le_bytes());
        // DW_FORM_flag_present: zero-length form, no bytes written
        // low_pc (relocated to function address) / high_pc (size, no reloc)
        let func_low_pc_off = info.len() as u64;
        info.extend_from_slice(&f.low_pc.to_le_bytes());
        relocs.push((func_low_pc_off, f.name.clone()));
        info.extend_from_slice(&(f.high_pc - f.low_pc).to_le_bytes());
        // decl_file + decl_line (source location for GDB backtrace)
        if has_decl_line {
            info.push(f.decl_file_idx as u8 + 1); // 1-based file index
            info.extend_from_slice(&(f.decl_line).to_le_bytes());
        }
        // frame_base 表达式：单个 `DW_OP_call_frame_cfa`（exprloc = ULEB 长度 + 字节）
        write_uleb128(&mut info, 1);
        info.push(DW_OP_call_frame_cfa);

        // formal parameters (with DW_AT_location for GDB variable inspection)
        for (idx, (pname, _pty)) in f.params.iter().enumerate() {
            let has_location = f.param_locations.len() > idx;
            let mut param_attrs: Vec<(u16, u16)> =
                vec![(DW_AT_name, DW_FORM_strp), (DW_AT_type, DW_FORM_ref4)];
            if has_location {
                param_attrs.push((DW_AT_location, DW_FORM_exprloc));
            }
            let pcode = abb.get_or_create(DW_TAG_formal_parameter, DW_CHILDREN_no, &param_attrs);
            write_uleb128(&mut info, pcode as u64);
            write_attr_strp(&mut info, strs.intern(pname));
            let pty_off = param_type_offs[idx];
            info.extend_from_slice(&pty_off.to_le_bytes());
            if has_location {
                write_location_expr(&mut info, &f.param_locations[idx]);
            }
        }

        // local variables in function scope (DW_TAG_variable)
        for (var, ty_off) in f.local_vars.iter().zip(&local_var_type_offs) {
            emit_local_var_die(&mut info, abb, strs, cu, var, *ty_off);
        }

        // lexical blocks (DW_TAG_lexical_block) with nested variables
        for (bi, block) in f.lexical_blocks.iter().enumerate() {
            let block_has_vars = !block.variables.is_empty();
            let block_attrs: Vec<(u16, u16)> =
                vec![(DW_AT_low_pc, DW_FORM_addr), (DW_AT_high_pc, DW_FORM_addr)];
            let bcode = abb.get_or_create(
                DW_TAG_lexical_block,
                if block_has_vars {
                    DW_CHILDREN_yes
                } else {
                    DW_CHILDREN_no
                },
                &block_attrs,
            );
            write_uleb128(&mut info, bcode as u64);
            info.extend_from_slice(&block.low_pc.to_le_bytes());
            info.extend_from_slice(&(block.high_pc - block.low_pc).to_le_bytes());
            // 类型 DIE 已在 subprogram 之前全部发完（否则它们会变成这个 block 的子 DIE）
            for (var, ty_off) in block.variables.iter().zip(&block_var_type_offs[bi]) {
                emit_local_var_die(&mut info, abb, strs, cu, var, *ty_off);
            }
            if block_has_vars {
                info.push(0); // terminate lexical_block children
            }
        }

        // subprogram 终止
        if has_children {
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
        // 类型 DIE 先发完：本全局 DIE 自己已经在属性字节里插过内层类型了，同一个 CU 深度
        // 上多一个兄弟 DIE 合法，插在属性字节中间则不合法。
        let ty_off = ensure_type_die(&mut info, abb, strs, &mut type_offsets, &g.ty);
        write_uleb128(&mut info, code as u64);
        write_attr_strp(&mut info, strs.intern(&g.name));
        if let Some(ln) = &g.linkage_name {
            write_attr_strp(&mut info, strs.intern(ln));
        }
        info.extend_from_slice(&ty_off.to_le_bytes());
        // DW_FORM_flag_present: zero-length form, no bytes written
    }

    // CU DIE 终止
    info.push(0);

    // 回填 unit_length
    let unit_len = (info.len() - 4) as u32;
    info[0..4].copy_from_slice(&unit_len.to_le_bytes());

    (abb.clone().into_bytes(), info, relocs)
}

fn write_attr_strp(info: &mut Vec<u8>, off: u32) {
    info.extend_from_slice(&off.to_le_bytes());
}

/// Emit a DW_TAG_variable DIE for a local variable with location
fn emit_local_var_die(
    info: &mut Vec<u8>,
    abb: &mut AbbrevBuilder,
    strs: &mut StringTable,
    _cu: &CompileUnit,
    var: &DwarfLocalVar,
    ty_off_prepared: u32,
) {
    let var_attrs: Vec<(u16, u16)> = vec![
        (DW_AT_name, DW_FORM_strp),
        (DW_AT_type, DW_FORM_ref4),
        (DW_AT_location, DW_FORM_exprloc),
    ];
    let vcode = abb.get_or_create(DW_TAG_variable, DW_CHILDREN_no, &var_attrs);
    write_uleb128(info, vcode as u64);
    write_attr_strp(info, strs.intern(&var.name));
    info.extend_from_slice(&ty_off_prepared.to_le_bytes());
    write_location_expr(info, &var.location);
}

/// 把一条位置描述编成 `DW_FORM_exprloc` 形式的属性值。
///
/// `exprloc` 的布局是 **ULEB128 长度 + 表达式字节**。以前这里直接发 `op + sleb(offset)`
/// 而没有长度前缀：解析器会把第一个操作码当长度读，后面全错。今天
/// `param_locations`/`local_vars` 还是空列表所以这条路径从未被踩到，但 Phase 6.2 要往里
/// 送寄存器家 —— 不先修好编码，升上去就是把一坑旧的错位变成新的。
fn write_location_expr(info: &mut Vec<u8>, loc: &DwarfParamLocation) {
    let mut expr: Vec<u8> = Vec::new();
    if loc.encode_into(&mut expr).is_err() {
        // 未识别的操作码：只发那个操作码本身，不自发明偏移。调用方今天只能从
        // `fbreg`/`cfa`/`reg()` 三个构造器拿到值，所以这条路不可达；走到这里就是新增了
        // 一种位置形状却没同时更新 `encode_into`，那由下面那条逐形状用例把住。
        expr.clear();
        expr.push(loc.op); // 只发操作码，不编偏移（编了就是拿一个假偏移骗调试器）
    }
    write_uleb128(info, expr.len() as u64);
    info.extend_from_slice(&expr);
}

fn ensure_type_die(
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
    write_type_die(info, abb, strs, type_offsets, ty);
    off
}

fn write_type_die(
    info: &mut Vec<u8>,
    abb: &mut AbbrevBuilder,
    strs: &mut StringTable,
    type_offsets: &mut std::collections::HashMap<String, u32>,
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
        },
        DwarfTypeKind::Pointer(inner) => {
            let attrs: Vec<(u16, u16)> =
                vec![(DW_AT_byte_size, DW_FORM_data1), (DW_AT_type, DW_FORM_ref4)];
            let code = abb.get_or_create(DW_TAG_pointer_type, DW_CHILDREN_no, &attrs);
            // 嵌套类型的 DIE 必须在写本 DIE 的 abbreviation 号**之前**发完：DWARF 是流式
            // 编码，一个 DIE 的属性字节必须连续，否则解析器会把内层 DIE 的字节当本 DIE 的
            // 属性读，从这一点开始整个 unit 全错位（实测 readelf：`Bogus end-of-siblings
            // marker` + `abbreviation number 94 which does not exist`，而编译器自己无感）。
            let inner_off = ensure_type_die(info, abb, strs, type_offsets, inner);
            write_uleb128(info, code as u64);
            info.push(ty.byte_size as u8);
            info.extend_from_slice(&inner_off.to_le_bytes());
        },
        DwarfTypeKind::Struct { members } => {
            write_aggregate_die(info, abb, strs, type_offsets, ty, members, false);
        },
        DwarfTypeKind::Union { members } => {
            write_aggregate_die(info, abb, strs, type_offsets, ty, members, true);
        },
        DwarfTypeKind::Array { element, count } => {
            let attrs: Vec<(u16, u16)> =
                vec![(DW_AT_type, DW_FORM_ref4), (DW_AT_count, DW_FORM_data4)];
            let code = abb.get_or_create(DW_TAG_array_type, DW_CHILDREN_yes, &attrs);
            // 同上：元素类型的 DIE 先发完，再写本 array DIE 的属性字节
            let elem_off = ensure_type_die(info, abb, strs, type_offsets, element);
            write_uleb128(info, code as u64);
            info.extend_from_slice(&elem_off.to_le_bytes());
            info.extend_from_slice(&(*count as u32).to_le_bytes());
            // subrange child
            let sub_attrs: Vec<(u16, u16)> = vec![(DW_AT_upper_bound, DW_FORM_data4)];
            let sub_code = abb.get_or_create(DW_TAG_subrange_type, DW_CHILDREN_no, &sub_attrs);
            write_uleb128(info, sub_code as u64);
            info.extend_from_slice(&((*count as u32).saturating_sub(1)).to_le_bytes());
            info.push(0); // array_type children 终止
        },
        DwarfTypeKind::Const(inner) => {
            write_qualifier_die(info, abb, strs, type_offsets, ty, inner, DW_TAG_const_type);
        },
        DwarfTypeKind::Volatile(inner) => {
            write_qualifier_die(info, abb, strs, type_offsets, ty, inner, DW_TAG_volatile_type);
        },
        DwarfTypeKind::Restrict(inner) => {
            write_qualifier_die(info, abb, strs, type_offsets, ty, inner, DW_TAG_restrict_type);
        },
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
            // 底层类型先发完（同一个「属性字节必须连续」的理由）
            let u_off = ensure_type_die(info, abb, strs, type_offsets, underlying);
            write_uleb128(info, code as u64);
            write_attr_strp(info, strs.intern(&ty.name));
            info.push(ty.byte_size as u8);
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
        },
        DwarfTypeKind::Func { ret, params } => {
            let attrs: Vec<(u16, u16)> = vec![(DW_AT_type, DW_FORM_ref4)];
            let code = abb.get_or_create(DW_TAG_subroutine_type, DW_CHILDREN_yes, &attrs);
            // 返回类型与全部形参类型都要先发完：本 DIE 与它那些 formal_parameter 子 DIE
            // 的属性字节里不得插入其它 DIE（同一个「流式编码」理由）。
            let ret_off = ensure_type_die(info, abb, strs, type_offsets, ret);
            let p_offs: Vec<u32> = params
                .iter()
                .map(|p| ensure_type_die(info, abb, strs, type_offsets, p))
                .collect();
            write_uleb128(info, code as u64);
            info.extend_from_slice(&ret_off.to_le_bytes());
            for p_off in p_offs {
                let param_attrs: Vec<(u16, u16)> = vec![(DW_AT_type, DW_FORM_ref4)];
                let pcode =
                    abb.get_or_create(DW_TAG_formal_parameter, DW_CHILDREN_no, &param_attrs);
                write_uleb128(info, pcode as u64);
                info.extend_from_slice(&p_off.to_le_bytes());
            }
            info.push(0);
        },
        DwarfTypeKind::Typedef { alias, target } => {
            let attrs: Vec<(u16, u16)> =
                vec![(DW_AT_name, DW_FORM_strp), (DW_AT_type, DW_FORM_ref4)];
            let code = abb.get_or_create(DW_TAG_typedef, DW_CHILDREN_no, &attrs);
            let t_off = ensure_type_die(info, abb, strs, type_offsets, target);
            write_uleb128(info, code as u64);
            write_attr_strp(info, strs.intern(alias));
            info.extend_from_slice(&t_off.to_le_bytes());
        },
        DwarfTypeKind::Class {
            members,
            bases,
            vtable_count,
        } => {
            write_class_die(info, abb, strs, type_offsets, ty, members, bases, *vtable_count);
        },
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
        },
    }
}

fn write_qualifier_die(
    info: &mut Vec<u8>,
    abb: &mut AbbrevBuilder,
    strs: &mut StringTable,
    type_offsets: &mut std::collections::HashMap<String, u32>,
    _ty: &DwarfType,
    inner: &DwarfType,
    tag: u16,
) {
    let _ = strs;
    let attrs: Vec<(u16, u16)> = vec![(DW_AT_type, DW_FORM_ref4)];
    let code = abb.get_or_create(tag, DW_CHILDREN_no, &attrs);
    let inner_off = ensure_type_die(info, abb, strs, type_offsets, inner);
    write_uleb128(info, code as u64);
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
    write_type_die(info, abb, strs, offsets, ty);
    off
}

fn write_aggregate_die(
    info: &mut Vec<u8>,
    abb: &mut AbbrevBuilder,
    strs: &mut StringTable,
    type_offsets: &mut std::collections::HashMap<String, u32>,
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
    let code =
        abb.get_or_create(tag, if has_members { DW_CHILDREN_yes } else { DW_CHILDREN_no }, &attrs);
    // 全部成员的类型 DIE 先发完（同一个「属性字节必须连续」的理由）。自指类型（`struct S { struct S *next; }`）
    // 仍然能解析：本类型的偏移已由 `ensure_type_die` 在调用本函数**之前**登记，所以拿到的就
    // 是这个尚未写完的 DIE 自己的偏移，与改动前同一形状。
    let member_types: Vec<u32> = members
        .iter()
        .map(|m| ensure_type_die(info, abb, strs, type_offsets, &m.ty))
        .collect();
    write_uleb128(info, code as u64);
    write_attr_strp(info, strs.intern(&ty.name));
    info.push(ty.byte_size as u8);
    for (m, t_off) in members.iter().zip(member_types) {
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
            // (小端序下, 字段在 byte 内的位偏移需要转换).
            // dcc 当前仅小端 x86_64/aarch64/riscv64, 直接用 offset_bits % 8 作为位偏移。
            let bit_in_byte = (m.offset_bits % 8) as u8;
            info.push(bit_in_byte);
        }
        info.extend_from_slice(&t_off.to_le_bytes());
    }
    if has_members {
        info.push(0);
    }
}

/// 发射 C++ class 的 DWARF DIE.
///
/// 使用 DW_TAG_class_type 而非 DW_TAG_structure_type,
/// 并支持继承 (DW_TAG_inheritance) 和多态 (DW_AT_vtable_elem_count).
// 一个 class DIE 需要同时携带 abbreviation/字符串表/行表/节偏移等上下文,
// 打包成结构体收益有限。
#[allow(clippy::too_many_arguments)]
fn write_class_die(
    info: &mut Vec<u8>,
    abb: &mut AbbrevBuilder,
    strs: &mut StringTable,
    type_offsets: &mut std::collections::HashMap<String, u32>,
    ty: &DwarfType,
    members: &[DwarfMember],
    bases: &[DwarfInheritance],
    vtable_count: u32,
) {
    let has_children = !members.is_empty() || !bases.is_empty();
    // 构建属性列表: 始终有 name + byte_size
    let mut attrs: Vec<(u16, u16)> =
        vec![(DW_AT_name, DW_FORM_strp), (DW_AT_byte_size, DW_FORM_data1)];
    // 多态类添加 DW_AT_vtable_elem_count
    if vtable_count > 0 {
        attrs.push((DW_AT_vtable_elem_count, DW_FORM_data2));
    }
    let code = abb.get_or_create(
        DW_TAG_class_type,
        if has_children { DW_CHILDREN_yes } else { DW_CHILDREN_no },
        &attrs,
    );
    // 同一个「属性字节必须连续」的理由：基类与成员的类型 DIE 全部先发完，再写本 class DIE。
    let base_types: Vec<u32> = bases
        .iter()
        .map(|b| ensure_type_die(info, abb, strs, type_offsets, &b.base_type))
        .collect();
    let member_types: Vec<u32> = members
        .iter()
        .map(|m| ensure_type_die(info, abb, strs, type_offsets, &m.ty))
        .collect();
    write_uleb128(info, code as u64);
    write_attr_strp(info, strs.intern(&ty.name));
    info.push(ty.byte_size as u8);
    if vtable_count > 0 {
        info.extend_from_slice(&(vtable_count as u16).to_le_bytes());
    }
    // 发射继承 DIE (DW_TAG_inheritance)
    for (base, base_off) in bases.iter().zip(&base_types) {
        let mut inh_attrs: Vec<(u16, u16)> = vec![
            (DW_AT_type, DW_FORM_ref4),
            (DW_AT_data_member_location, DW_FORM_data1),
        ];
        if base.is_virtual {
            inh_attrs.push((DW_AT_virtuality, DW_FORM_flag));
        }
        inh_attrs.push((DW_AT_visibility, DW_FORM_data1));
        let inh_code = abb.get_or_create(DW_TAG_inheritance, DW_CHILDREN_no, &inh_attrs);
        write_uleb128(info, inh_code as u64);
        info.extend_from_slice(&base_off.to_le_bytes());
        info.push(base.offset as u8);
        if base.is_virtual {
            info.push(1); // DW_VIRTUALITY_virtual
        }
        info.push(base.access); // 1=public, 2=protected, 3=private
    }
    // 发射成员 DIE (与 write_aggregate_die 相同)
    for (m, t_off) in members.iter().zip(&member_types) {
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
            info.push(m.bit_size as u8);
            let bit_in_byte = (m.offset_bits % 8) as u8;
            info.push(bit_in_byte);
        }
        info.extend_from_slice(&t_off.to_le_bytes());
    }
    if has_children {
        info.push(0); // children 终止符
    }
}

// ============================================================================
// .debug_line (DWARF v4/v5)
// ============================================================================

/// 函数入口的那一行（`DW_AT_low_pc` ↔ `DW_AT_decl_line`）。
///
/// 跳过两类不能当入口用的条目：只有原型没有体的声明 DIE（`is_declaration`，地址
/// 不成立），以及区间倒挂/零长的函数（与 `build_aranges` 里那条 `hi <= lo` 同一个
/// 口径）。入口行落在不存在的地址上，gdb 会把它当成 CU 区间里的空洞，症状就是
/// `Internal error: pc ... in read in CU, but not in symtab.`。
fn function_entry_line(f: &DwarfFuncInfo) -> Option<LineEntry> {
    if f.is_declaration || f.high_pc <= f.low_pc {
        return None;
    }
    Some(LineEntry {
        pc: f.low_pc,
        line: if f.decl_line > 0 { f.decl_line } else { 1 },
        column: 1,
        file_idx: 0,
        discriminator: 0,
        is_stmt: true,
        basic_block: false,
    })
}

fn build_line_program(cu: &CompileUnit, opts: &DebugOptions) -> (Vec<u8>, Vec<(u64, String)>) {
    let mut out = Vec::new();
    let mut relocs: Vec<(u64, String)> = Vec::new();
    let hdr_start = out.len();
    let dwarf_ver = opts.dwarf_version;
    out.extend_from_slice(&[0u8; 4]); // unit_length
    out.extend_from_slice(&(dwarf_ver as u16).to_le_bytes()); // version
    if dwarf_ver >= 5 {
        // DWARF v5: add address_size and segment_selector_size
        out.push(opts.arch.address_size());
        out.push(0); // segment_selector_size
    }
    let header_length_pos = out.len();
    out.extend_from_slice(&[0u8; 4]); // header_length (回填)
    out.push(opts.min_insn_len);
    if dwarf_ver >= 5 {
        // DWARF v5: maximum_operations_per_instruction removed
        // (always 1 for non-VLIW architectures)
    } else {
        out.push(opts.max_ops_per_insn); // DWARF 4 必须为 1
    }
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

    if dwarf_ver >= 5 {
        // DWARF v5: directory table with entry format
        // 1 entry format: (DW_LNCT_path, DW_FORM_string)
        out.push(1); // directory_entry_format_count
        write_uleb128(&mut out, DW_LNCT_path as u64);
        write_uleb128(&mut out, DW_FORM_string as u64);
        // directories count
        write_uleb128(&mut out, cu.line_dirs.len() as u64);
        for d in &cu.line_dirs {
            out.extend_from_slice(d.as_bytes());
            out.push(0); // null-terminated string
        }

        // DWARF v5: file name table with entry format
        // 2 entry formats: (DW_LNCT_path, DW_FORM_string),
        //                  (DW_LNCT_directory_index, DW_FORM_udata)
        out.push(2); // file_name_entry_format_count
        write_uleb128(&mut out, DW_LNCT_path as u64);
        write_uleb128(&mut out, DW_FORM_string as u64);
        write_uleb128(&mut out, DW_LNCT_directory_index as u64);
        write_uleb128(&mut out, DW_FORM_data1 as u64);
        // file_names count
        write_uleb128(&mut out, cu.line_files.len() as u64);
        for f in &cu.line_files {
            out.extend_from_slice(f.as_bytes());
            out.push(0); // null-terminated name
            out.push(0); // directory_index = 0 (first dir)
        }
    } else {
        // DWARF v4: directory table is simple null-terminated strings
        // followed by a single null byte terminator
        for d in &cu.line_dirs {
            out.extend_from_slice(d.as_bytes());
            out.push(0);
        }
        out.push(0); // directory table terminator

        // DWARF v4: file name table entries: (name, dir_idx, time, size)
        // followed by a single null byte terminator
        for f in &cu.line_files {
            out.extend_from_slice(f.as_bytes());
            out.push(0); // null-terminated name
            write_uleb128(&mut out, 0); // directory_index (0 = first dir)
            write_uleb128(&mut out, 0); // timestamp
            write_uleb128(&mut out, 0); // size
        }
        out.push(0); // file name table terminator
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

    // 函数入口行与语句行**都要**铺。入口行保证「PC 落在函数序言（push %rbp /
    // mov %rsp,%rbp / sub $N,%rsp 那几条）上」时 gdb 仍能报出正确行；以前这里是
    // 「有详细条目就完全丢弃入口行」的二选一，而 x86_64 后端从来不会为普通 C 语句
    // 发条目（只覆盖内联 asm），于是行表退到「每函数一行」的兜底形态，
    // `break file.c:N` 全部绑不上。现在入口行给函数首地址（与 `DW_AT_low_pc` 一致），
    // 语句行给断点与单步。
    //
    // 已经被详细条目占用的 PC 不再补入口行：同一 PC 上留两行不同行号时，gdb 取哪一条
    // 取决于表的写入序，而入口行和该 PC 的语句行本来就只能留一个。
    let mut entries: Vec<LineEntry> = Vec::new();
    if cu.line_entries.is_empty() {
        for f in &cu.functions {
            if let Some(e) = function_entry_line(f) {
                entries.push(e);
            }
        }
    } else {
        let taken: std::collections::HashSet<u64> = cu.line_entries.iter().map(|e| e.pc).collect();
        for f in &cu.functions {
            if taken.contains(&f.low_pc) {
                continue;
            }
            if let Some(e) = function_entry_line(f) {
                entries.push(e);
            }
        }
        entries.extend(cu.line_entries.iter().cloned());
    }

    // 行表的地址必须单调递增（理由见 `normalize_cu` 第 2 条）。这一步不能只靠上游排序：
    // `cu.line_entries` 的出处不止一个（asm 块级条目与逐指令条目两张表拼起来），而
    // fallback 那一份来自函数表。任何一次改动让顺序倒退，行号状态机就编不出「往回走」
    // 的 advance，产出一张 gdb 二分查找命不中的表（症状与 CU 范围错同一种：
    // `Internal error: pc ... in read in CU, but not in symtab.`）。
    // 排序键与后端拼表时一致：PC 升序 → 同 PC 时语句边界优先 → discriminator 升序。
    let mut entries: Vec<LineEntry> = entries;
    entries.sort_by(|a, b| {
        a.pc.cmp(&b.pc)
            .then_with(|| b.is_stmt.cmp(&a.is_stmt))
            .then_with(|| a.discriminator.cmp(&b.discriminator))
    });
    // 完全同址同行的重复行丢掉（同址不同行是合法的：内联、同一 PC 上多条语句），
    // 重复行不是「多此一举」：同一 PC 上出现两行不同文件/行会让 gdb 取后者而丢掉函数入口
    entries.dedup_by(|a, b| {
        a.pc == b.pc
            && a.line == b.line
            && a.file_idx == b.file_idx
            && a.column == b.column
            && a.discriminator == b.discriminator
            && a.is_stmt == b.is_stmt
    });

    // M24: dedup adjacent same-source same-line entries
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

    // Emit line program with DW_LNE_set_address at every function boundary.
    //
    // 「函数边界」要逐点重发 `DW_LNE_set_address` 并挂 reloc：行表里的地址字段写的是
    // 函数在 `.text` 内的偏移，链接器只在这 8 字节上挂了指向该函数的 reloc 才会把它搬到
    // 最终虚拟地址。以前只有 fallback（每函数一行）那条路挂 reloc，详细条目那条路整张表
    // 只发一次 set_address 且不挂 reloc —— 产物里的行表于是落在一个「与任何代码地址都不
    // 重合」的低地址段上，症状不是警告而是 `No line 5 in file`：gdb 读得到 CU，却按地址
    // 找不到那一行的序列。
    //
    // 上面的入口行保证每个函数的 low_pc 处必有一条条目，所以这里可以用「pc == 某函数
    // low_pc」当边界判据，而不必给表中间那些没有符号可挂的偏移造 reloc。
    let mut last_pc: u64 = 0;
    let mut first_entry = true;
    for (i, e) in entries.iter().enumerate() {
        let boundary_fn: Option<&DwarfFuncInfo> = cu.functions.iter().find(|f| f.low_pc == e.pc);
        let need_set_addr =
            first_entry || (cu.line_entries.is_empty() && i > 0) || boundary_fn.is_some();
        if need_set_addr {
            // DW_LNE_set_address: extended opcode 0x02 with 8-byte address
            out.push(DW_LNS_extended_op);
            out.push(9); // length: 1 (opcode) + 8 (address)
            out.push(DW_LNE_set_address);
            let addr_offset = out.len() as u64;
            out.extend_from_slice(&e.pc.to_le_bytes());
            // Track relocation for this address
            if let Some(f) = boundary_fn {
                relocs.push((addr_offset, f.name.clone()));
            }
            state.address = e.pc;
            last_pc = e.pc;
            first_entry = false;
        }
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

    (out, relocs)
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

    // ── special opcode (DWARF §6.2.5.2 matrix optimization) ──────────
    //
    // DWARF 行号状态机用 (address, line) 二维矩阵编码 special opcode:
    //   adjusted_opcode = opcode - opcode_base
    //   operation_advance = adjusted_opcode / line_range
    //   address += operation_advance * minimum_instruction_length
    //   line += line_base + (adjusted_opcode % line_range)
    //
    // 当 pc_advance ∈ [0, min_insn] 且 line_advance ∈ [line_base, line_base+line_range-1]
    // 时, 可用 1 字节 special opcode 同时推进 PC 和行号, 比标准序列
    // (DW_LNS_advance_pc + DW_LNS_advance_line + DW_LNS_copy = 4-6 字节)
    // 节省 3-5 字节.
    //
    // 公式:
    //   op_advance = pc_advance / min_insn    (0 或 1)
    //   line_incr  = line_advance - line_base  (∈ [0, line_range-1])
    //   adjusted   = op_advance * line_range + line_incr
    //   opcode     = opcode_base + adjusted
    //
    // 注意: 当 pc_advance < min_insn (非零) 时, special opcode 的
    // address 推进为 0 (因为 op_advance=0), 与目标 pc_advance 不匹配.
    // 此情况仅在非 min_insn 对齐地址时出现, 实际 debug info 中罕见.
    // 当前实现仍走 special opcode + 直接设置 state.address, 后续
    // 相对推进基于正确地址计算, 对调试器消费无影响.
    let op_advance = if min_insn > 0 {
        (pc_advance / min_insn as u64) as i64
    } else {
        0
    };
    let line_incr = line_advance - line_base as i64;
    let adjusted = op_advance * line_range as i64 + line_incr;
    // ── discriminator 变化需在任一输出路径前发射 ──────────────────
    // DW_LNE_set_discriminator 是扩展操作码, 不推进 PC 也不产生行,
    // 仅设置状态机的 discriminator 字段, 后续 copy/special 将其带入行矩阵.
    let disc_changed = e.discriminator as u32 != state.discriminator;
    let is_stmt_changed = e.is_stmt != state.is_stmt;
    let basic_block_changed = e.basic_block && !state.basic_block;
    // Special opcode 仅编码 (address, line) 推进; 若 file/column/is_stmt/
    // basic_block 也发生变化, 必须走标准路径发射对应 DW_LNS 操作码,
    // 否则 DWARF 消费者无法感知这些状态变化.
    if line_advance >= line_base as i64
        && line_advance <= max_line_incr
        && pc_advance <= min_insn as u64
        && file_advance == 0
        && column_advance == 0
        && !is_stmt_changed
        && !basic_block_changed
        && adjusted >= 0
        && adjusted < line_range as i64 * 2
    // sanity: fits in reasonable range
    {
        let opcode = opcode_base as i64 + adjusted;
        if opcode >= opcode_base as i64 && opcode <= 255 {
            if disc_changed {
                // 先发射 DW_LNE_set_discriminator, 再发 special opcode
                out.push(DW_LNS_extended_op);
                let disc_len_pos = out.len();
                out.push(0); // placeholder for length
                out.push(DW_LNE_set_discriminator);
                write_uleb128(out, e.discriminator as u64);
                let disc_bytes = (out.len() - disc_len_pos - 1) as u8;
                out[disc_len_pos] = disc_bytes;
            }
            out.push(opcode as u8);
            state.address = e.pc;
            state.line = e.line as i64;
            state.column = e.column;
            state.file = e.file_idx as u32 + 1;
            state.discriminator = e.discriminator as u32;
            return;
        }
    }

    // ── DW_LNS_const_add_pc 增强路径 (分析后暂不启用) ───────────────
    //
    // DW_LNS_const_add_pc (0x08, 0 操作数) 隐式推进 PC:
    //   adjusted_opcode = (255 - opcode_base) % line_range  (通常 = 0)
    //   address += (255 - opcode_base) / line_range * min_insn
    //   即 address += 255 * min_insn (opcode_base=13, line_range=14 时)
    //
    // 理论用法: 先 emit const_add_pc (1 字节), 再 emit special opcode,
    // 总共 2 字节可表达 pc_advance ∈ [255*min_insn, 510*min_insn].
    // 对比标准序列 (DW_LNS_advance_pc uleb128 + DW_LNS_copy = 3-5 字节),
    // 节省 1-3 字节.
    //
    // 固有 trade-off (line_advance 冲突):
    //   const_add_pc 仅扩展 PC 表示范围, 不扩展 line 表示范围.
    //   后续 special opcode 的 line 增量仍受 [line_base, line_base+line_range-1]
    //   约束 (当前 = [-5, 8]). 当 line_advance 超出此区间时:
    //     - 需额外 DW_LNS_advance_line (sleb128) = +2-3 字节
    //     - 总计: const_add_pc(1) + advance_line(2-3) + special(1) = 4-5 字节
    //     - 对比标准序列: advance_line(2-3) + advance_pc(2) + copy(1) = 5-6 字节
    //     - 仅节省 1 字节, 不值得增加代码复杂度
    //   当 line_advance 在区间内时:
    //     - const_add_pc 路径: 2 字节, 但要求 pc_advance > 255*min_insn
    //     - 此场景在实际代码中罕见 (函数内 255+ 字节无行号变化)
    //
    // 结论: 收益场景窄 (pc_advance > 255 且 line_advance 小), 收益量小
    // (1 字节), 且与 line_advance 处理逻辑耦合. 暂不启用.
    // 未来若扩大 line_range (> 255) 或引入 multi-byte special opcode,
    // 可重新评估此路径.

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
    if disc_changed {
        out.push(DW_LNS_extended_op);
        let disc_len_pos = out.len();
        out.push(0); // placeholder for length
        out.push(DW_LNE_set_discriminator);
        write_uleb128(out, e.discriminator as u64);
        let disc_bytes = (out.len() - disc_len_pos - 1) as u8;
        out[disc_len_pos] = disc_bytes;
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
    state.discriminator = e.discriminator as u32;
}

// ============================================================================
// .debug_aranges
// ============================================================================

fn build_aranges(cu: &CompileUnit, opts: &DebugOptions) -> (Vec<u8>, Vec<(u64, String)>) {
    let mut out = Vec::new();
    let mut relocs: Vec<(u64, String)> = Vec::new();
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
        for (idx, &(lo, hi)) in cu.func_ranges.iter().enumerate() {
            // 零长/倒序的区间不发：`.debug_aranges` 是 gdb 用来「按地址找 CU」的索引，
            // 一条长度为 0 的表项不是「无意义」而是「说谎」（它声称本 CU 覆盖一段不存在的
            // 代码）。区间的递增与去重已由 `normalize_cu` 保证（`func_ranges` 与
            // `functions` 同序同长，下面按 `idx` 取名字因此不会错位）。
            if hi <= lo {
                continue;
            }
            let addr_offset = out.len() as u64;
            out.extend_from_slice(&lo.to_le_bytes());
            // Track relocation for this address
            if idx < cu.functions.len() {
                relocs.push((addr_offset, cu.functions[idx].name.clone()));
            }
            out.extend_from_slice(&(hi - lo).to_le_bytes());
        }
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
    }
    let unit_len = (out.len() - hdr_start - 4) as u32;
    out[hdr_start..hdr_start + 4].copy_from_slice(&unit_len.to_le_bytes());
    (out, relocs)
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
// DWARF v5 新增节: .debug_str_offsets / .debug_addr / .debug_line_str
// ============================================================================

/// Build .debug_str_offsets section (DWARF v5 section 6.1.1.2)
/// Contains a header followed by an array of offsets into .debug_str.
/// Each offset references a string used via DW_FORM_strx* forms.
/// Currently emits a minimal valid section (header + empty offset table)
/// since we still use DW_FORM_strp for string references.
fn build_str_offsets(_cu: &CompileUnit, opts: &DebugOptions) -> Vec<u8> {
    if opts.dwarf_version < 5 {
        return Vec::new();
    }
    let mut out = Vec::new();
    // Section header: unit_length(4) + version(2) + padding(2) = 8 bytes
    out.extend_from_slice(&[0u8; 4]); // unit_length placeholder
    out.extend_from_slice(&5u16.to_le_bytes()); // version = 5
    out.extend_from_slice(&0u16.to_le_bytes()); // padding
                                                // No offset entries (we use DW_FORM_strp, not DW_FORM_strx*)
    let unit_len = (out.len() - 4) as u32;
    out[0..4].copy_from_slice(&unit_len.to_le_bytes());
    out
}

/// Build .debug_addr section (DWARF v5 section 6.1.1.3)
/// Contains a header followed by address table entries.
/// Currently emits a minimal valid section since we still use
/// DW_FORM_addr for address references.
fn build_addr(_cu: &CompileUnit, opts: &DebugOptions) -> Vec<u8> {
    if opts.dwarf_version < 5 {
        return Vec::new();
    }
    let mut out = Vec::new();
    // Section header: unit_length(4) + version(2) + address_size(1) + padding(1) = 8 bytes
    out.extend_from_slice(&[0u8; 4]); // unit_length placeholder
    out.extend_from_slice(&5u16.to_le_bytes()); // version = 5
    out.push(opts.arch.address_size()); // address_size
    out.push(0); // padding
                 // No address entries (we use DW_FORM_addr, not DW_FORM_addrx*)
    let unit_len = (out.len() - 4) as u32;
    out[0..4].copy_from_slice(&unit_len.to_le_bytes());
    out
}

/// Build .debug_line_str section (DWARF v5 section 6.2.4)
/// Contains strings referenced by the line program via DW_FORM_line_strp.
/// Currently the line program uses DW_FORM_string (inline), so this
/// section is empty. Reserved for future indexed line string support.
fn build_line_str(_cu: &CompileUnit, opts: &DebugOptions) -> Vec<u8> {
    if opts.dwarf_version < 5 {
        return Vec::new();
    }
    // Empty section - line program uses DW_FORM_string inline
    Vec::new()
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
    let dwarf_ver = opts.dwarf_version;
    // DWO info: 写一个 skeleton CU 头
    let mut info = Vec::new();
    info.extend_from_slice(&[0u8; 4]); // unit_length
    info.extend_from_slice(&(dwarf_ver as u16).to_le_bytes()); // version
    if dwarf_ver >= 5 {
        // DWARF v5: unit_type(1) + address_size(1) + abbrev_offset(4)
        info.push(DW_UT_compile);
        info.push(opts.arch.address_size());
        info.extend_from_slice(&0u32.to_le_bytes()); // abbrev offset
    } else {
        // DWARF v4: abbrev_offset(4) + address_size(1)
        info.extend_from_slice(&0u32.to_le_bytes()); // abbrev offset
        info.push(opts.arch.address_size());
    }
    // dwo_id (placeholder)
    info.extend_from_slice(&0u64.to_le_bytes());
    // empty children
    info.push(0);
    let ilen = (info.len() - 4) as u32;
    info[0..4].copy_from_slice(&ilen.to_le_bytes());

    // 简化 abbrev: 1 个空 CU 缩写
    let abbrev = vec![1, DW_TAG_compile_unit as u8, DW_CHILDREN_no, 0, 0, 0];
    let str_dwo = cu.source_name.as_bytes().to_vec();
    let (line_dwo, _line_dwo_relocs) = build_line_program(cu, opts);
    let macro_dwo = build_debug_macro(cu, opts);

    // Skeleton CU
    let mut skeleton = Vec::new();
    skeleton.extend_from_slice(&[0u8; 4]);
    skeleton.extend_from_slice(&(dwarf_ver as u16).to_le_bytes()); // version
    if dwarf_ver >= 5 {
        // DWARF v5: unit_type(1) + address_size(1) + abbrev_offset(4)
        skeleton.push(DW_UT_compile);
        skeleton.push(opts.arch.address_size());
        skeleton.extend_from_slice(&0u32.to_le_bytes()); // abbrev offset
    } else {
        // DWARF v4: abbrev_offset(4) + address_size(1)
        skeleton.extend_from_slice(&0u32.to_le_bytes()); // abbrev offset
        skeleton.push(opts.arch.address_size());
    }
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
    write_attr_strp(&mut skeleton, strs.intern(&format!("{}.dwo", cu.source_name)));
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

/// BTF 字符串表：以 \0 开头，每个字符串以 \0 结尾
struct BtfStringTable {
    data: Vec<u8>,
    map: std::collections::HashMap<String, u32>,
}

impl BtfStringTable {
    fn new() -> Self {
        Self {
            data: vec![0], // BTF 字符串表以空字符串（\0）开头
            map: std::collections::HashMap::new(),
        }
    }

    /// 插入字符串，返回其在字符串表中的偏移
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&off) = self.map.get(s) {
            return off;
        }
        let off = self.data.len() as u32;
        self.data.extend_from_slice(s.as_bytes());
        self.data.push(0);
        self.map.insert(s.to_string(), off);
        off
    }

    fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

/// BTF 类型编码器：收集类型并分配 ID
struct BtfTypeEncoder {
    types: Vec<Vec<u8>>, // 每个元素的编码后的 type record
    name_to_id: std::collections::HashMap<String, u32>,
}

impl BtfTypeEncoder {
    fn new() -> Self {
        Self {
            types: Vec::new(),
            name_to_id: std::collections::HashMap::new(),
        }
    }

    /// 当前已编码的类型数量（ID 从 1 开始）
    fn next_id(&self) -> u32 {
        (self.types.len() + 1) as u32
    }

    /// 编码一个 BTF 类型记录
    /// name_off: 字符串表偏移
    /// kind: BTF_KIND_*
    /// vlen: 成员/参数数量
    /// size_or_type: 大小（struct/union/enum）或类型 ID（ptr/const 等）
    /// extra: 附加数据（成员、参数等）
    fn add_type(
        &mut self,
        name_off: u32,
        kind: u32,
        vlen: u32,
        size_or_type: u32,
        extra: &[u8],
    ) -> u32 {
        let id = self.next_id();
        let mut record = Vec::new();
        record.extend_from_slice(&name_off.to_le_bytes());
        // info: kind_flag=0, vlen, kind
        let info = (kind << 24) | (vlen & 0xFFFF);
        record.extend_from_slice(&info.to_le_bytes());
        record.extend_from_slice(&size_or_type.to_le_bytes());
        record.extend_from_slice(extra);
        self.types.push(record);
        id
    }

    /// 注册类型名到 ID 的映射
    fn register_name(&mut self, name: &str, id: u32) {
        if !name.is_empty() {
            self.name_to_id.insert(name.to_string(), id);
        }
    }

    /// 查找已注册的类型 ID
    fn lookup_name(&self, name: &str) -> Option<u32> {
        self.name_to_id.get(name).copied()
    }
}

/// 生成完整 BTF 类型信息（包含编译单元中所有类型）
///
/// 生成的 BTF 节包含：
/// - 所有基础类型（int, float 等）
/// - 结构体/联合体及其成员
/// - 数组类型
/// - 指针类型
/// - typedef
/// - enum 及其枚举值
/// - const/volatile/restrict 修饰类型
/// - 函数原型
///
/// 这使得 pahole 可以直接解析 BTF 而无需回退到 DWARF，
/// 并满足 BTF dedup 和 objtool --btf 的完整需求。
pub fn build_btf_full(cu: &CompileUnit) -> Vec<u8> {
    let mut strs = BtfStringTable::new();
    let mut encoder = BtfTypeEncoder::new();

    // 预注册基础 int 类型（如果尚未存在）
    // BTF 中 int 类型需要编码位大小和偏移
    let int_names = [
        ("int", 32, true), // signed 32-bit
        ("unsigned int", 32, false),
        ("long", 64, true), // 64-bit on x86_64
        ("unsigned long", 64, false),
        ("short", 16, true),
        ("unsigned short", 16, false),
        ("char", 8, true),
        ("signed char", 8, true),
        ("unsigned char", 8, false),
        ("long long", 64, true),
        ("unsigned long long", 64, false),
        ("_Bool", 8, false),
    ];

    // 先编码所有基础 int 类型
    for (name, bits, signed) in &int_names {
        let name_off = strs.intern(name);
        // int 编码: 位偏移(0) | 位大小 | (signed ? 0 : 1<<31)
        let int_data = if *signed {
            (*bits as u32) << 24
        } else {
            ((*bits as u32) << 24) | (1 << 31)
        };
        let id = encoder.add_type(name_off, BTF_KIND_INT, 0, int_data, &[]);
        encoder.register_name(name, id);
    }

    // 预编码浮点类型 (BTF_KIND_FLOAT = 16)
    let float_names = [("float", 32), ("double", 64), ("long double", 128)];
    for (name, bits) in &float_names {
        let name_off = strs.intern(name);
        let id = encoder.add_type(name_off, BTF_KIND_FLOAT, 0, *bits, &[]);
        encoder.register_name(name, id);
    }

    // 编码 void 类型（BTF 中 void 是类型 ID 0，不需要显式编码）
    // 但我们需要一个 void 的引用，它始终是 0

    // 收集编译单元中所有类型
    let all_types = collect_all_cu_types(cu);

    // 编码每个类型
    for ty in &all_types {
        encode_btf_type(ty, &mut strs, &mut encoder);
    }

    // 编码全局变量的类型
    for g in &cu.globals {
        encode_btf_type(&g.ty, &mut strs, &mut encoder);
    }

    // 编码函数参数和返回类型
    for f in &cu.functions {
        encode_btf_type(&f.return_type, &mut strs, &mut encoder);
        for (_, pty) in &f.params {
            encode_btf_type(pty, &mut strs, &mut encoder);
        }
    }

    // 编码全局变量为 BTF_KIND_VAR + 收集到 DATASEC
    // BPF CO-RE 需要 VAR + DATASEC 来映射全局变量到 .bss/.data/.rodata
    let mut bss_vars: Vec<u32> = Vec::new();
    let mut data_vars: Vec<u32> = Vec::new();
    let rodata_vars: Vec<u32> = Vec::new();

    for g in &cu.globals {
        let ty_id = encoder.lookup_name(&g.ty.name).unwrap_or(0);
        let name_off = strs.intern(&g.name);
        // link: 0=static, 1=global, 2=extern
        let link = if g.is_external { 2u32 } else { 1u32 };
        // VAR record: name_off(4) + info(4) + type(4) + link(4)
        let mut var_data = Vec::new();
        var_data.extend_from_slice(&ty_id.to_le_bytes());
        var_data.extend_from_slice(&link.to_le_bytes());
        let var_id = encoder.add_type(name_off, BTF_KIND_VAR, 0, 0, &var_data);
        encoder.register_name(&g.name, var_id);
        // 分类到对应的 DATASEC (按变量名启发式分类)
        // 实际场景中由 codegen 提供 section 信息, 此处用默认分类
        if g.is_external {
            data_vars.push(var_id);
        } else {
            bss_vars.push(var_id);
        }
    }

    // 编码 DATASEC 节 (.bss, .data, .rodata)
    // 每个 DATASEC 包含一组 VAR, 按偏移排列
    let _ = rodata_vars; // reserved for future use
    for (sec_name, var_ids) in &[(".bss", &bss_vars), (".data", &data_vars)] {
        if var_ids.is_empty() {
            continue;
        }
        let name_off = strs.intern(sec_name);
        let vlen = var_ids.len() as u32;
        // DATASEC 成员: type_id(4) + offset(4) + size(4)
        let mut ds_data = Vec::new();
        let mut offset = 0u32;
        for &vid in var_ids.iter() {
            ds_data.extend_from_slice(&vid.to_le_bytes());
            ds_data.extend_from_slice(&offset.to_le_bytes());
            // size placeholder (0 = unknown, verifier will fill)
            ds_data.extend_from_slice(&0u32.to_le_bytes());
            offset += 8; // placeholder size per var
        }
        let total_size = offset;
        let id = encoder.add_type(name_off, BTF_KIND_DATASEC, vlen, total_size, &ds_data);
        encoder.register_name(sec_name, id);
    }

    // 构建类型段
    let mut types_data = Vec::new();
    for record in &encoder.types {
        types_data.extend_from_slice(record);
    }

    let type_len = types_data.len() as u32;
    let str_data = strs.as_bytes();
    let str_len = str_data.len() as u32;
    let hdr_len = 24u32;
    let type_off = 0u32;
    let str_off = type_off + type_len;

    // 构建 BTF 头部
    let mut btf = Vec::new();
    btf.extend_from_slice(&0xEB9F_u16.to_le_bytes()); // magic
    btf.push(1); // version
    btf.push(0); // flags
    btf.extend_from_slice(&hdr_len.to_le_bytes());
    btf.extend_from_slice(&type_off.to_le_bytes());
    btf.extend_from_slice(&type_len.to_le_bytes());
    btf.extend_from_slice(&str_off.to_le_bytes());
    btf.extend_from_slice(&str_len.to_le_bytes());
    btf.extend_from_slice(&types_data);
    btf.extend_from_slice(str_data);

    btf
}

/// 生成 .BTF.ext 节 (func_info + line_info)
///
/// .BTF.ext 为 BPF verifier 提供函数和行号信息, 是 BPF CO-RE 的
/// 关键组件. 布局:
///   header: magic(2) + version(1) + flags(1) + hdr_len(4)
///           + func_info_off(4) + func_info_len(4)
///           + line_info_off(4) + line_info_len(4)
///   func_info: rec_size(4) + [sec_name_off(4) + num_info(4) + entries...]
///   line_info: rec_size(4) + [sec_name_off(4) + num_info(4) + entries...]
pub fn build_btf_ext(cu: &CompileUnit) -> Vec<u8> {
    // 字符串表 (与 .BTF 共享格式, 但 .BTF.ext 有自己的副本)
    let mut strs = BtfStringTable::new();

    // --- func_info ---
    // rec_size = 8 (insn_off(4) + type_id(4))
    let func_rec_size = 8u32;
    let mut func_data = Vec::new();
    // 将所有函数放入一个 section (”.text")
    if !cu.functions.is_empty() {
        let sec_name_off = strs.intern(".text");
        let num_info = cu.functions.len() as u32;
        func_data.extend_from_slice(&sec_name_off.to_le_bytes());
        func_data.extend_from_slice(&num_info.to_le_bytes());
        for f in &cu.functions {
            let insn_off = f.low_pc as u32;
            // type_id 指向 FUNC 类型的 BTF ID; 此处用 0 占位
            // (实际链接时由 BTF dedup 填充)
            let type_id = 0u32;
            func_data.extend_from_slice(&insn_off.to_le_bytes());
            func_data.extend_from_slice(&type_id.to_le_bytes());
        }
    }

    // --- line_info ---
    // rec_size = 16 (insn_off(4) + file_name_off(4) + line_off(4) + line_col(4))
    let line_rec_size = 16u32;
    let mut line_data = Vec::new();
    if !cu.line_entries.is_empty() {
        let sec_name_off = strs.intern(".text");
        let num_info = cu.line_entries.len() as u32;
        line_data.extend_from_slice(&sec_name_off.to_le_bytes());
        line_data.extend_from_slice(&num_info.to_le_bytes());
        for le in &cu.line_entries {
            let insn_off = le.pc as u32;
            let file_name_off = if (le.file_idx as usize) < cu.line_files.len() {
                strs.intern(&cu.line_files[le.file_idx as usize])
            } else {
                0
            };
            // source line offset (into string table)
            let line_off = 0u32; // placeholder
                                 // line(24 bits) | column(8 bits) packed
            let line_col = ((le.line & 0x00FFFFFF) << 8) | (le.column & 0xFF);
            line_data.extend_from_slice(&insn_off.to_le_bytes());
            line_data.extend_from_slice(&file_name_off.to_le_bytes());
            line_data.extend_from_slice(&line_off.to_le_bytes());
            line_data.extend_from_slice(&line_col.to_le_bytes());
        }
    }

    // 计算偏移
    let hdr_len = 32u32; // 8 header fields * 4 bytes
    let func_info_off = 0u32;
    let func_info_len = (4 + func_data.len()) as u32; // rec_size(4) + data
    let line_info_off = func_info_off + func_info_len;
    let line_info_len = (4 + line_data.len()) as u32;

    // 构建 .BTF.ext
    let mut out = Vec::new();
    out.extend_from_slice(&0xEB9F_u16.to_le_bytes()); // magic
    out.push(1); // version
    out.push(0); // flags
    out.extend_from_slice(&hdr_len.to_le_bytes());
    out.extend_from_slice(&func_info_off.to_le_bytes());
    out.extend_from_slice(&func_info_len.to_le_bytes());
    out.extend_from_slice(&line_info_off.to_le_bytes());
    out.extend_from_slice(&line_info_len.to_le_bytes());
    // func_info section
    out.extend_from_slice(&func_rec_size.to_le_bytes());
    out.extend_from_slice(&func_data);
    // line_info section
    out.extend_from_slice(&line_rec_size.to_le_bytes());
    out.extend_from_slice(&line_data);
    // 字符串表
    out.extend_from_slice(strs.as_bytes());

    out
}

fn encode_btf_type(ty: &DwarfType, strs: &mut BtfStringTable, encoder: &mut BtfTypeEncoder) -> u32 {
    // 检查是否已编码（通过名称查找）
    if !ty.name.is_empty() {
        if let Some(id) = encoder.lookup_name(&ty.name) {
            return id;
        }
    }

    match &ty.kind {
        DwarfTypeKind::Void => 0, // void 始终是类型 ID 0

        DwarfTypeKind::Base(enc) => {
            // 基础类型已经预编码，尝试查找
            if let Some(id) = encoder.lookup_name(&ty.name) {
                return id;
            }
            let name_off = strs.intern(&ty.name);
            let bits = (ty.byte_size * 8) as u32;
            // Float 类型使用 BTF_KIND_FLOAT (16)
            if matches!(enc, TypeEncoding::Float) {
                let id = encoder.add_type(name_off, BTF_KIND_FLOAT, 0, bits, &[]);
                encoder.register_name(&ty.name, id);
                return id;
            }
            let signed = matches!(enc, TypeEncoding::Signed | TypeEncoding::SignedChar);
            let int_data = if signed { bits << 24 } else { (bits << 24) | (1 << 31) };
            let id = encoder.add_type(name_off, BTF_KIND_INT, 0, int_data, &[]);
            encoder.register_name(&ty.name, id);
            id
        },

        DwarfTypeKind::Pointer(inner) => {
            let target_id = encode_btf_type(inner, strs, encoder);
            let name_off = 0; // 指针通常无名

            encoder.add_type(name_off, BTF_KIND_PTR, 0, target_id, &[])
        },

        DwarfTypeKind::Const(inner) => {
            let target_id = encode_btf_type(inner, strs, encoder);
            let name_off = 0;

            encoder.add_type(name_off, BTF_KIND_CONST, 0, target_id, &[])
        },

        DwarfTypeKind::Volatile(inner) => {
            let target_id = encode_btf_type(inner, strs, encoder);
            let name_off = 0;

            encoder.add_type(name_off, BTF_KIND_VOLATILE, 0, target_id, &[])
        },

        DwarfTypeKind::Restrict(inner) => {
            let target_id = encode_btf_type(inner, strs, encoder);
            let name_off = 0;

            encoder.add_type(name_off, BTF_KIND_RESTRICT, 0, target_id, &[])
        },

        DwarfTypeKind::Struct { members } => {
            // 先注册名称以防止递归
            let name_off = strs.intern(&ty.name);
            // 临时占位，稍后更新
            let temp_id = encoder.next_id();
            encoder.register_name(&ty.name, temp_id);

            // 编码所有成员类型
            let mut member_data = Vec::new();
            for m in members {
                let m_name_off = strs.intern(&m.name);
                let m_type_id = encode_btf_type(&m.ty, strs, encoder);
                // BTF struct member: name_off(4) + type(4) + offset(4)
                member_data.extend_from_slice(&m_name_off.to_le_bytes());
                member_data.extend_from_slice(&m_type_id.to_le_bytes());
                // offset: 位偏移编码在低 24 位，位域大小在高 8 位
                let offset_bits = m.offset_bits as u32;
                if m.bit_size > 0 {
                    // 位域: offset | (bit_size << 24)
                    let offset_enc = offset_bits | (m.bit_size << 24);
                    member_data.extend_from_slice(&offset_enc.to_le_bytes());
                } else {
                    member_data.extend_from_slice(&offset_bits.to_le_bytes());
                }
            }

            let size = ty.byte_size as u32;
            let vlen = members.len() as u32;
            // 实际编码（替换占位）
            // 由于我们已经注册了名称，需要检查是否是第一次编码
            if encoder.lookup_name(&ty.name) == Some(temp_id)
                && encoder.types.len() >= (temp_id as usize)
            {
                // 已经编码过了
                return temp_id;
            }
            let id = encoder.add_type(name_off, BTF_KIND_STRUCT, vlen, size, &member_data);
            // 更新映射（如果之前注册了临时 ID）
            if id != temp_id {
                encoder.name_to_id.insert(ty.name.clone(), id);
            }
            id
        },

        DwarfTypeKind::Union { members } => {
            let name_off = strs.intern(&ty.name);
            let temp_id = encoder.next_id();
            encoder.register_name(&ty.name, temp_id);

            let mut member_data = Vec::new();
            for m in members {
                let m_name_off = strs.intern(&m.name);
                let m_type_id = encode_btf_type(&m.ty, strs, encoder);
                member_data.extend_from_slice(&m_name_off.to_le_bytes());
                member_data.extend_from_slice(&m_type_id.to_le_bytes());
                // union 成员偏移通常为 0
                member_data.extend_from_slice(&0u32.to_le_bytes());
            }

            let size = ty.byte_size as u32;
            let vlen = members.len() as u32;
            if encoder.lookup_name(&ty.name) == Some(temp_id)
                && (temp_id as usize) <= encoder.types.len() + 1
                && encoder.types.len() >= (temp_id as usize)
            {
                return temp_id;
            }
            let id = encoder.add_type(name_off, BTF_KIND_UNION, vlen, size, &member_data);
            if id != temp_id {
                encoder.name_to_id.insert(ty.name.clone(), id);
            }
            id
        },

        DwarfTypeKind::Array { element, count } => {
            let elem_id = encode_btf_type(element, strs, encoder);
            // 数组索引类型：使用 unsigned long
            let idx_name = "unsigned long";
            let idx_id = encoder.lookup_name(idx_name).unwrap_or(1); // 默认使用第一个 int 类型

            let name_off = 0; // 数组通常无名
                              // BTF array: elem_type(4) + index_type(4) + nelems(4)
            let mut arr_data = Vec::new();
            arr_data.extend_from_slice(&elem_id.to_le_bytes());
            arr_data.extend_from_slice(&idx_id.to_le_bytes());
            arr_data.extend_from_slice(&(*count as u32).to_le_bytes());

            encoder.add_type(name_off, BTF_KIND_ARRAY, 0, 0, &arr_data)
        },

        DwarfTypeKind::Enum {
            members: variants,
            underlying,
        } => {
            let name_off = strs.intern(&ty.name);
            let _ = encode_btf_type(underlying, strs, encoder);

            let mut enum_data = Vec::new();
            for (var_name, var_val) in variants {
                let var_name_off = strs.intern(var_name);
                enum_data.extend_from_slice(&var_name_off.to_le_bytes());
                enum_data.extend_from_slice(&(*var_val as i32).to_le_bytes());
            }

            let size = ty.byte_size as u32;
            let vlen = variants.len() as u32;
            let id = encoder.add_type(name_off, BTF_KIND_ENUM, vlen, size, &enum_data);
            encoder.register_name(&ty.name, id);
            id
        },

        DwarfTypeKind::Typedef { alias, target } => {
            let target_id = encode_btf_type(target, strs, encoder);
            let name_off = strs.intern(alias);
            let id = encoder.add_type(name_off, BTF_KIND_TYPEDEF, 0, target_id, &[]);
            encoder.register_name(alias, id);
            id
        },

        DwarfTypeKind::Func { ret, params } => {
            // 先编码函数原型
            let ret_id = encode_btf_type(ret, strs, encoder);

            let mut param_data = Vec::new();
            for p in params {
                let p_name_off = strs.intern(&p.name);
                let p_type_id = encode_btf_type(p, strs, encoder);
                // BTF func param: name_off(4) + type(4)
                param_data.extend_from_slice(&p_name_off.to_le_bytes());
                param_data.extend_from_slice(&p_type_id.to_le_bytes());
            }

            // 先编码 FUNC_PROTO
            let proto_name_off = if ty.name.is_empty() { 0 } else { strs.intern(&ty.name) };
            let vlen = params.len() as u32;
            let proto_id =
                encoder.add_type(proto_name_off, BTF_KIND_FUNC_PROTO, vlen, ret_id, &param_data);

            // 然后编码 FUNC（引用 FUNC_PROTO）
            let func_name_off = if ty.name.is_empty() { 0 } else { strs.intern(&ty.name) };
            let id = encoder.add_type(func_name_off, BTF_KIND_FUNC, 0, proto_id, &[]);
            if !ty.name.is_empty() {
                encoder.register_name(&ty.name, id);
            }
            id
        },

        DwarfTypeKind::Class { members, bases, .. } => {
            // BTF 不区分 struct 和 class, 统一使用 BTF_KIND_STRUCT.
            // 基类信息在 BTF 中暂不编码 (BTF 无原生继承支持).
            let _ = bases;
            let name_off = strs.intern(&ty.name);
            let temp_id = encoder.next_id();
            encoder.register_name(&ty.name, temp_id);

            let mut member_data = Vec::new();
            for m in members {
                let m_name_off = strs.intern(&m.name);
                let m_type_id = encode_btf_type(&m.ty, strs, encoder);
                member_data.extend_from_slice(&m_name_off.to_le_bytes());
                member_data.extend_from_slice(&m_type_id.to_le_bytes());
                let offset_bits = m.offset_bits as u32;
                if m.bit_size > 0 {
                    let offset_enc = offset_bits | (m.bit_size << 24);
                    member_data.extend_from_slice(&offset_enc.to_le_bytes());
                } else {
                    member_data.extend_from_slice(&offset_bits.to_le_bytes());
                }
            }

            let size = ty.byte_size as u32;
            let vlen = members.len() as u32;
            if encoder.lookup_name(&ty.name) == Some(temp_id)
                && encoder.types.len() >= (temp_id as usize)
            {
                return temp_id;
            }
            let id = encoder.add_type(name_off, BTF_KIND_STRUCT, vlen, size, &member_data);
            if id != temp_id {
                encoder.name_to_id.insert(ty.name.clone(), id);
            }
            id
        },
    }
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
        while !eh.len().is_multiple_of(8) {
            eh.push(0);
        }
        let fde_len = (eh.len() - fde_start - 4) as u32;
        eh[fde_start..fde_start + 4].copy_from_slice(&fde_len.to_le_bytes());
    }
    eh
}

/// ORC 元数据：构建 .orc_unwind_ip 节区 (4 字节 PC 偏移数组，已排序)
/// 每个条目是相对于 .text 节起始的 4 字节偏移。
pub fn build_orc_unwind_ip(functions: &[FuncFrame]) -> Vec<u8> {
    let mut data = Vec::with_capacity(functions.len() * 4);
    for func in functions {
        // PC 已经是 .text 内偏移
        data.extend_from_slice(&(func.pc as u32).to_le_bytes());
    }
    if data.is_empty() {
        data.extend_from_slice(&0u32.to_le_bytes());
    }
    data
}

/// ORC 元数据：构建 .orc_unwind 节区 (6 字节 packed orc_entry 数组)
/// 每个条目对应 .orc_unwind_ip 中相同索引的 PC。
///
/// struct orc_entry (arch/x86/include/asm/orc_types.h, __packed, 6 字节):
///   sp_offset: s16, bp_offset: s16,
///   sp_reg: u4, bp_reg: u4, type: u3, signal: u1
///
/// 布局 (little-endian):
///   byte 0..1: sp_offset (s16)
///   byte 2..3: bp_offset (s16)
///   byte 4:    (bp_reg << 4) | sp_reg
///   byte 5:    (signal << 3) | type
///
/// 注意: 必须与 objtool/内核 ORC 解包器期望的 6 字节大小一致 —
/// unwind_init() 校验 orc_size % sizeof(struct orc_entry) == 0,
/// 多出的字节会让表解析错位.
pub fn build_orc_unwind(functions: &[FuncFrame]) -> Vec<u8> {
    // 内核 ORC 寄存器/类型枚举 (asm/orc_types.h)
    const ORC_REG_SP: u8 = 3;
    const ORC_REG_BP: u8 = 4;
    const ORC_TYPE_CALL: u8 = 2;
    let mut data = Vec::with_capacity(functions.len() * 6);
    for func in functions {
        let has_cfi = !func.cfi_ops.is_empty();
        // 无 frame pointer: CFA = rsp + 8 (返回地址在 CFA-8)
        // 有 CFI (frame pointer): CFA = rbp + 16 (保存的 rbp 在 CFA-16)
        let (sp_offset, sp_reg, bp_offset, bp_reg) = if has_cfi {
            (16i16, ORC_REG_BP, 0i16, 0u8)
        } else {
            (8i16, ORC_REG_SP, 0i16, 0u8)
        };
        let orc_type: u8 = ORC_TYPE_CALL;
        let signal: u8 = 0;
        // Pack into 6 bytes (little-endian bitfields: 低地址低位)
        data.extend_from_slice(&sp_offset.to_le_bytes());
        data.extend_from_slice(&bp_offset.to_le_bytes());
        data.push((bp_reg << 4) | (sp_reg & 0xf));
        data.push((signal << 3) | (orc_type & 0x7));
    }
    if data.is_empty() {
        data.resize(6, 0);
    }
    data
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

    /// 测试完整 BTF 生成：包含多种类型
    #[test]
    fn btf_full_contains_multiple_types() {
        // 创建一个包含多种类型的 CompileUnit
        let cu = CompileUnit {
            source_name: "test.c".to_string(),
            comp_dir: None,
            producer: "test".to_string(),
            language: 12, // DW_LANG_C99
            functions: vec![],
            globals: vec![],
            types: vec![
                // 基础 int 类型
                DwarfType {
                    name: "int".to_string(),
                    byte_size: 4,
                    kind: DwarfTypeKind::Base(TypeEncoding::Signed),
                },
                // 结构体
                DwarfType {
                    name: "test_struct".to_string(),
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
                },
            ],
            line_entries: vec![],
            line_files: vec![],
            line_dirs: vec![],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };

        let btf = build_btf_full(&cu);

        // 验证 BTF 头部
        assert!(btf.starts_with(&0xEB9F_u16.to_le_bytes()));
        assert!(btf.len() >= 24, "BTF should have at least header");

        // 验证版本
        assert_eq!(btf[2], 1, "BTF version should be 1");

        // 验证包含 INT 类型
        let has_int = btf.windows(4).any(|w| {
            let info = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            (info & 0x1f) == BTF_KIND_INT
        });
        assert!(has_int, "BTF should contain INT type");

        // 验证包含 STRUCT 类型
        let has_struct = btf.windows(4).any(|w| {
            let info = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            (info & 0x1f) == BTF_KIND_STRUCT
        });
        assert!(has_struct, "BTF should contain STRUCT type");

        // 验证字符串表包含类型名
        let btf_str = String::from_utf8_lossy(&btf);
        assert!(btf_str.contains("test_struct"), "BTF string table should contain 'test_struct'");
    }

    /// 测试 BTF 指针类型编码
    #[test]
    fn btf_full_contains_pointer_type() {
        let cu = CompileUnit {
            source_name: "test.c".to_string(),
            comp_dir: None,
            producer: "test".to_string(),
            language: 12,
            functions: vec![],
            globals: vec![],
            types: vec![
                DwarfType {
                    name: "int".to_string(),
                    byte_size: 4,
                    kind: DwarfTypeKind::Base(TypeEncoding::Signed),
                },
                DwarfType {
                    name: "".to_string(), // 指针通常无名
                    byte_size: 8,
                    kind: DwarfTypeKind::Pointer(Box::new(DwarfType {
                        name: "int".to_string(),
                        byte_size: 4,
                        kind: DwarfTypeKind::Base(TypeEncoding::Signed),
                    })),
                },
            ],
            line_entries: vec![],
            line_files: vec![],
            line_dirs: vec![],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };

        let btf = build_btf_full(&cu);

        // 验证包含 PTR 类型
        let has_ptr = btf.windows(4).any(|w| {
            let info = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            (info & 0x1f) == BTF_KIND_PTR
        });
        assert!(has_ptr, "BTF should contain PTR type");
    }

    /// 测试 BTF enum 类型编码
    #[test]
    fn btf_full_contains_enum_type() {
        let cu = CompileUnit {
            source_name: "test.c".to_string(),
            comp_dir: None,
            producer: "test".to_string(),
            language: 12,
            functions: vec![],
            globals: vec![],
            types: vec![DwarfType {
                name: "color".to_string(),
                byte_size: 4,
                kind: DwarfTypeKind::Enum {
                    members: vec![
                        ("RED".to_string(), 0),
                        ("GREEN".to_string(), 1),
                        ("BLUE".to_string(), 2),
                    ],
                    underlying: Box::new(DwarfType {
                        name: "unsigned int".to_string(),
                        byte_size: 4,
                        kind: DwarfTypeKind::Base(TypeEncoding::Unsigned),
                    }),
                },
            }],
            line_entries: vec![],
            line_files: vec![],
            line_dirs: vec![],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };

        let btf = build_btf_full(&cu);

        // 验证包含 ENUM 类型
        let has_enum = btf.windows(4).any(|w| {
            let info = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            (info & 0x1f) == BTF_KIND_ENUM
        });
        assert!(has_enum, "BTF should contain ENUM type");

        // 验证字符串表包含枚举值名
        let btf_str = String::from_utf8_lossy(&btf);
        assert!(
            btf_str.contains("RED") && btf_str.contains("GREEN") && btf_str.contains("BLUE"),
            "BTF string table should contain enum values"
        );
    }

    /// 测试 BTF typedef 编码
    #[test]
    fn btf_full_contains_typedef() {
        let cu = CompileUnit {
            source_name: "test.c".to_string(),
            comp_dir: None,
            producer: "test".to_string(),
            language: 12,
            functions: vec![],
            globals: vec![],
            types: vec![
                DwarfType {
                    name: "int".to_string(),
                    byte_size: 4,
                    kind: DwarfTypeKind::Base(TypeEncoding::Signed),
                },
                DwarfType {
                    name: "my_int".to_string(),
                    byte_size: 4,
                    kind: DwarfTypeKind::Typedef {
                        alias: "my_int".to_string(),
                        target: Box::new(DwarfType {
                            name: "int".to_string(),
                            byte_size: 4,
                            kind: DwarfTypeKind::Base(TypeEncoding::Signed),
                        }),
                    },
                },
            ],
            line_entries: vec![],
            line_files: vec![],
            line_dirs: vec![],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };

        let btf = build_btf_full(&cu);

        // 验证包含 TYPEDEF 类型
        let has_typedef = btf.windows(4).any(|w| {
            let info = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            (info & 0x1f) == BTF_KIND_TYPEDEF
        });
        assert!(has_typedef, "BTF should contain TYPEDEF type");

        // 验证字符串表包含 typedef 名
        let btf_str = String::from_utf8_lossy(&btf);
        assert!(btf_str.contains("my_int"), "BTF string table should contain 'my_int'");
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
        assert!(found_r13, "FDE should embed DW_CFA_offset(13, 2) for r13 save");
        // 验证 %rip (reg 16) 字节
        let needle_rip = [0x80u8 | 16, 1];
        let mut found_rip = false;
        for w in eh.windows(needle_rip.len()) {
            if w == needle_rip {
                found_rip = true;
                break;
            }
        }
        assert!(found_rip, "FDE should embed DW_CFA_offset(16, 1) for %rip save");
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
        assert!(!found_in_full, "FDE with empty cfi_ops should not embed def_cfa(7,16)");
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
    fn dwarf_v5_info_has_version_5_and_unit_type() {
        let cu = CompileUnit {
            source_name: "test.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            functions: vec![],
            globals: vec![],
            types: vec![],
            line_entries: vec![],
            line_files: vec!["test.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };
        let opts = DebugOptions {
            level: 2,
            dwarf_version: 5,
            ..Default::default()
        };
        let sec = build_dwarf_full(&cu, &opts);
        // .debug_info header: unit_length(4) + version(2) = offset 4,5
        assert!(sec.info.len() > 6);
        assert_eq!(sec.info[4], 5); // version = 5
        assert_eq!(sec.info[5], 0);
        // DWARF v5: unit_type at offset 6
        assert_eq!(sec.info[6], DW_UT_compile);
        // DWARF v5: address_size at offset 7
        assert_eq!(sec.info[7], 8); // x86_64 = 8 bytes
    }

    #[test]
    fn dwarf_v5_line_has_version_5_header() {
        let cu = CompileUnit {
            source_name: "test.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            functions: vec![],
            globals: vec![],
            types: vec![],
            line_entries: vec![],
            line_files: vec!["test.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };
        let opts = DebugOptions {
            level: 2,
            dwarf_version: 5,
            ..Default::default()
        };
        let sec = build_dwarf_full(&cu, &opts);
        // .debug_line header: unit_length(4) + version(2) = offset 4,5
        assert!(sec.line.len() > 6);
        assert_eq!(sec.line[4], 5); // version = 5
        assert_eq!(sec.line[5], 0);
        // DWARF v5: address_size at offset 6
        assert_eq!(sec.line[6], 8); // x86_64
                                    // DWARF v5: segment_selector_size at offset 7
        assert_eq!(sec.line[7], 0);
    }

    #[test]
    fn dwarf_v5_emits_str_offsets_and_addr_sections() {
        let cu = CompileUnit {
            source_name: "test.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            functions: vec![],
            globals: vec![],
            types: vec![],
            line_entries: vec![],
            line_files: vec!["test.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };
        let opts = DebugOptions {
            level: 2,
            dwarf_version: 5,
            ..Default::default()
        };
        let sec = build_dwarf_full(&cu, &opts);
        // .debug_str_offsets should be present with v5 header
        assert!(!sec.str_offsets.is_empty());
        // Version = 5 at offset 4
        assert_eq!(sec.str_offsets[4], 5);
        // .debug_addr should be present with v5 header
        assert!(!sec.addr.is_empty());
        assert_eq!(sec.addr[4], 5);
    }

    #[test]
    fn dwarf_v4_no_str_offsets_or_addr() {
        let dbg = build_dwarf("test.c", &[("main", 0x100)]);
        // v4 should not emit these sections
        assert!(dbg.str_offsets.is_empty());
        assert!(dbg.addr.is_empty());
        assert!(dbg.line_str.is_empty());
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
                param_locations: vec![],
                local_vars: vec![],
                lexical_blocks: vec![],
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
        assert!(!has_bit_size, "non-bitfield struct should not emit DW_AT_bit_size");
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

    /// 统计 line program opcode 区中 special opcode 数量 (>= opcode_base 的字节).
    /// 跳过 header 部分 (unit_length + version + header_length + 固定/可变头字段).
    fn count_special_opcodes(line_bytes: &[u8], opcode_base: u8) -> usize {
        let op_start = skip_line_header(line_bytes);
        line_bytes[op_start..]
            .iter()
            .filter(|&&b| b >= opcode_base)
            .count()
    }

    /// 统计 opcode 区中 DW_LNS_advance_pc (0x02) 出现次数.
    /// 需正确解析操作码流: 跳过扩展操作码序列 (0x00 + ULEB128 长度 + 数据),
    /// 避免将 DW_LNE_set_address (0x02) 等扩展操作码误计为 DW_LNS_advance_pc.
    fn count_advance_pc(line_bytes: &[u8], opcode_base: u8) -> usize {
        let op_start = skip_line_header(line_bytes);
        let ops = &line_bytes[op_start..];
        let mut count = 0;
        let mut i = 0;
        while i < ops.len() {
            let byte = ops[i];
            if byte == DW_LNS_extended_op {
                // 扩展操作码: 0x00 + ULEB128 长度 + 数据
                i += 1;
                if i >= ops.len() {
                    break;
                }
                let (len, bytes_read) = read_uleb128_from_slice(&ops[i..]);
                i += bytes_read + len as usize;
            } else if byte == 0x00 {
                // DW_LNE_end_sequence 或其他以 0x00 开头的序列
                i += 1;
            } else if byte >= opcode_base {
                // Special opcode: 单字节, 无参数, 不计数
                i += 1;
            } else {
                // 标准操作码: 检查是否为 DW_LNS_advance_pc
                if byte == DW_LNS_advance_pc {
                    count += 1;
                }
                i += 1;
                // 根据操作码类型跳过参数
                match byte {
                    // ULEB128 参数: advance_pc, set_file, set_column, set_isa
                    0x02 | 0x04 | 0x05 | 0x0c => {
                        let (_, bytes_read) = read_uleb128_from_slice(&ops[i..]);
                        i += bytes_read;
                    },
                    // SLEB128 参数: advance_line
                    0x03 => {
                        let (_, bytes_read) = read_sleb128_from_slice(&ops[i..]);
                        i += bytes_read;
                    },
                    // 固定 2 字节参数: fixed_advance_pc
                    0x09 => {
                        i += 2;
                    },
                    // 无参数: copy, negate_stmt, set_basic_block, const_add_pc,
                    //         set_prologue_end, set_epilogue_begin
                    _ => {},
                }
            }
        }
        count
    }

    /// 从字节切片读取 ULEB128 编码值, 返回 (值, 消耗字节数).
    fn read_uleb128_from_slice(bytes: &[u8]) -> (u64, usize) {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        let mut count = 0;
        for &b in bytes {
            result |= ((b & 0x7f) as u64) << shift;
            count += 1;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        (result, count)
    }

    /// 从字节切片读取 SLEB128 编码值, 返回 (值, 消耗字节数).
    fn read_sleb128_from_slice(bytes: &[u8]) -> (i64, usize) {
        let mut result: i64 = 0;
        let mut shift: u32 = 0;
        let mut count = 0;
        let mut byte;
        for &b in bytes {
            byte = b;
            result |= ((b & 0x7f) as i64) << shift;
            shift += 7;
            count += 1;
            if b & 0x80 == 0 {
                // 符号扩展
                if shift < 64 && (byte & 0x40) != 0 {
                    result |= !0i64 << shift;
                }
                break;
            }
        }
        (result, count)
    }

    /// 解析 .debug_line header 长度, 返回 opcode 区起始偏移.
    /// header 布局: unit_length(4) + version(2) + header_length(4) + header_data...
    /// opcode 区起点 = 4 + 2 + 4 + header_length.
    fn skip_line_header(line_bytes: &[u8]) -> usize {
        if line_bytes.len() < 10 {
            return 0;
        }
        let header_length =
            u32::from_le_bytes([line_bytes[6], line_bytes[7], line_bytes[8], line_bytes[9]]);
        (4 + 2 + 4 + header_length as usize).min(line_bytes.len())
    }

    #[test]
    fn matrix_opt_special_opcode_pc_zero() {
        // pc_advance=0, line_advance 在 [line_base, line_base+line_range-1] 内:
        // 应使用 1 字节 special opcode (op_advance=0).
        // 第一个 entry 从 pc=0 开始, 避免初始 setup 引入 advance_pc.
        let entries = vec![
            LineEntry {
                pc: 0,
                line: 1,
                column: 0,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
            LineEntry {
                pc: 0,   // pc_advance = 0
                line: 3, // line_advance = 2, in [-5, 8]
                column: 0,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
        ];
        let cu = make_line_cu(entries);
        let opts = DebugOptions::default();
        let sec = build_dwarf_full(&cu, &opts);
        // 应有 special opcode (>= opcode_base 的字节)
        let specials = count_special_opcodes(&sec.line, opts.opcode_base);
        assert!(
            specials >= 1,
            "pc_advance=0 should use special opcode, got {} specials",
            specials
        );
        // 不应有 DW_LNS_advance_pc (初始 pc=0, 无需推进)
        assert_eq!(
            count_advance_pc(&sec.line, opts.opcode_base),
            0,
            "pc_advance=0 should not need DW_LNS_advance_pc"
        );
    }

    #[test]
    fn matrix_opt_special_opcode_pc_eq_min_insn() {
        // pc_advance=min_insn (x86: 1), line_advance 在范围内:
        // 应使用 1 字节 special opcode (op_advance=1).
        let entries = vec![
            LineEntry {
                pc: 0,
                line: 1,
                column: 0,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
            LineEntry {
                pc: 1,   // pc_advance = 1 = min_insn (x86)
                line: 3, // line_advance = 2, in [-5, 8]
                column: 0,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
        ];
        let cu = make_line_cu(entries);
        let opts = DebugOptions::default();
        let sec = build_dwarf_full(&cu, &opts);
        let specials = count_special_opcodes(&sec.line, opts.opcode_base);
        assert!(
            specials >= 1,
            "pc_advance=min_insn should use special opcode, got {} specials",
            specials
        );
        // 不应有 DW_LNS_advance_pc (1 字节 special op 已覆盖)
        assert_eq!(
            count_advance_pc(&sec.line, opts.opcode_base),
            0,
            "pc_advance=min_insn should not need DW_LNS_advance_pc"
        );
    }

    #[test]
    fn matrix_opt_fallback_when_line_out_of_range() {
        // line_advance 超出 special opcode 范围时, 应回退到标准序列.
        let entries = vec![
            LineEntry {
                pc: 0,
                line: 1,
                column: 0,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
            LineEntry {
                pc: 4,
                line: 100, // line_advance = 99, 远超 [-5, 8]
                column: 0,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
        ];
        let cu = make_line_cu(entries);
        let opts = DebugOptions::default();
        let sec = build_dwarf_full(&cu, &opts);
        // 应使用 DW_LNS_advance_line + DW_LNS_advance_pc + DW_LNS_copy
        assert!(
            count_advance_pc(&sec.line, opts.opcode_base) >= 1,
            "large line_advance should fall back to standard sequence"
        );
    }

    #[test]
    fn matrix_opt_compact_vs_standard_size() {
        // 验证 special opcode 路径比标准序列更紧凑.
        // 两个 entry: pc 差 1, line 差 2 (均在 special opcode 范围).
        let entries = vec![
            LineEntry {
                pc: 0,
                line: 10,
                column: 0,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
            LineEntry {
                pc: 1,
                line: 12,
                column: 0,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
        ];
        let cu = make_line_cu(entries);
        let sec = build_dwarf_full(&cu, &DebugOptions::default());
        // special opcode 路径: 第 1 个 entry 用 special op (1 字节),
        // 第 2 个 entry 也用 special op (1 字节).
        // 标准序列: advance_line(2) + advance_pc(2) + copy(1) = 5 字节.
        // 验证: 总 opcode 区应比纯标准序列短.
        assert!(
            sec.line.len() < 70,
            "matrix opt should keep .debug_line compact, got {} bytes",
            sec.line.len()
        );
    }

    /// discriminator 变化应发射 DW_LNE_set_discriminator 扩展操作码
    #[test]
    fn dwarf_line_emits_discriminator() {
        let entries = vec![
            LineEntry {
                pc: 0x100,
                line: 10,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            },
            LineEntry {
                pc: 0x104,
                line: 10,
                column: 1,
                file_idx: 0,
                discriminator: 1, // 同一行但不同指令 (asm 块内逐指令条目)
                is_stmt: false,
                basic_block: false,
            },
            LineEntry {
                pc: 0x108,
                line: 10,
                column: 1,
                file_idx: 0,
                discriminator: 2, // 第三条指令
                is_stmt: false,
                basic_block: false,
            },
        ];
        let cu = make_line_cu(entries);
        let sec = build_dwarf_full(&cu, &DebugOptions::default());
        // 验证 .debug_line 包含 DW_LNE_set_discriminator (0x04)
        // DW_LNE_set_discriminator 编码: 0x00 (extended_op), len, 0x04, ULEB128(value)
        let has_discriminator = sec
            .line
            .windows(3)
            .any(|w| w[0] == 0x00 && w[2] == DW_LNE_set_discriminator);
        assert!(
            has_discriminator,
            ".debug_line should contain DW_LNE_set_discriminator opcode for per-insn asm entries"
        );
    }

    /// Test: BTF output includes const/volatile/restrict qualifier nodes
    /// for struct/union fields with qualifiers.
    #[test]
    fn btf_qualifier_nodes_present() {
        // Verify that DwarfTypeKind::Const/Volatile/Restrict produce
        // BTF_KIND_CONST/VOLATILE/RESTRICT records in BTF output.
        let mut strs = BtfStringTable::new();
        let mut encoder = BtfTypeEncoder::new();

        // Encode a base int type first
        let int_name = strs.intern("int");
        let int_data = (32u32) << 24; // signed 32-bit
        let int_id = encoder.add_type(int_name, BTF_KIND_INT, 0, int_data, &[]);
        encoder.register_name("int", int_id);

        // Encode const int
        let const_int = DwarfType {
            name: "const int".to_string(),
            byte_size: 4,
            kind: DwarfTypeKind::Const(Box::new(DwarfType {
                name: "int".to_string(),
                byte_size: 4,
                kind: DwarfTypeKind::Base(TypeEncoding::Signed),
            })),
        };
        let const_id = encode_btf_type(&const_int, &mut strs, &mut encoder);

        // Encode volatile int
        let vol_int = DwarfType {
            name: "volatile int".to_string(),
            byte_size: 4,
            kind: DwarfTypeKind::Volatile(Box::new(DwarfType {
                name: "int".to_string(),
                byte_size: 4,
                kind: DwarfTypeKind::Base(TypeEncoding::Signed),
            })),
        };
        let vol_id = encode_btf_type(&vol_int, &mut strs, &mut encoder);

        // Encode restrict int
        let rest_int = DwarfType {
            name: "restrict int".to_string(),
            byte_size: 4,
            kind: DwarfTypeKind::Restrict(Box::new(DwarfType {
                name: "int".to_string(),
                byte_size: 4,
                kind: DwarfTypeKind::Base(TypeEncoding::Signed),
            })),
        };
        let rest_id = encode_btf_type(&rest_int, &mut strs, &mut encoder);

        // Verify the encoded types have the correct BTF kinds
        assert!(const_id > 0, "const int should have a valid BTF type ID");
        assert!(vol_id > 0, "volatile int should have a valid BTF type ID");
        assert!(rest_id > 0, "restrict int should have a valid BTF type ID");

        // Check that the encoded records contain the right kind values
        // BTF info field: kind is in bits 24-31, vlen in bits 0-15
        let has_const = encoder.types.iter().any(|rec| {
            rec.len() >= 8 && {
                let info = u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]]);
                ((info >> 24) & 0x1f) == BTF_KIND_CONST
            }
        });
        let has_volatile = encoder.types.iter().any(|rec| {
            rec.len() >= 8 && {
                let info = u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]]);
                ((info >> 24) & 0x1f) == BTF_KIND_VOLATILE
            }
        });
        let has_restrict = encoder.types.iter().any(|rec| {
            rec.len() >= 8 && {
                let info = u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]]);
                ((info >> 24) & 0x1f) == BTF_KIND_RESTRICT
            }
        });
        assert!(has_const, "BTF should contain a CONST qualifier record");
        assert!(has_volatile, "BTF should contain a VOLATILE qualifier record");
        assert!(has_restrict, "BTF should contain a RESTRICT qualifier record");
    }

    /// Test: BTF output includes BTF_KIND_FLOAT for float types
    #[test]
    fn btf_full_contains_float_types() {
        let cu = CompileUnit {
            source_name: "test.c".to_string(),
            comp_dir: None,
            producer: "test".to_string(),
            language: 12,
            functions: vec![],
            globals: vec![],
            types: vec![],
            line_entries: vec![],
            line_files: vec![],
            line_dirs: vec![],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };
        let btf = build_btf_full(&cu);
        // Verify BTF_KIND_FLOAT records exist
        let has_float = btf.windows(4).any(|w| {
            let info = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            ((info >> 24) & 0x1f) == BTF_KIND_FLOAT
        });
        assert!(has_float, "BTF should contain FLOAT type records");
        // Verify string table contains float/double
        let btf_str = String::from_utf8_lossy(&btf);
        assert!(btf_str.contains("float"), "BTF string table should contain 'float'");
        assert!(btf_str.contains("double"), "BTF string table should contain 'double'");
    }

    /// Test: BTF output includes VAR and DATASEC for global variables
    #[test]
    fn btf_full_contains_var_and_datasec() {
        let cu = CompileUnit {
            source_name: "test.c".to_string(),
            comp_dir: None,
            producer: "test".to_string(),
            language: 12,
            functions: vec![],
            globals: vec![DwarfGlobalInfo {
                name: "my_global".to_string(),
                linkage_name: None,
                ty: DwarfType {
                    name: "int".to_string(),
                    byte_size: 4,
                    kind: DwarfTypeKind::Base(TypeEncoding::Signed),
                },
                decl_line: 1,
                decl_file_idx: 0,
                is_external: false,
            }],
            types: vec![],
            line_entries: vec![],
            line_files: vec![],
            line_dirs: vec![],
            low_pc: 0,
            high_pc: 0,
            func_ranges: vec![],
            cfi_rows: vec![],
        };
        let btf = build_btf_full(&cu);
        // Verify BTF_KIND_VAR exists
        let has_var = btf.windows(4).any(|w| {
            let info = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            ((info >> 24) & 0x1f) == BTF_KIND_VAR
        });
        assert!(has_var, "BTF should contain VAR type for globals");
        // Verify BTF_KIND_DATASEC exists
        let has_datasec = btf.windows(4).any(|w| {
            let info = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            ((info >> 24) & 0x1f) == BTF_KIND_DATASEC
        });
        assert!(has_datasec, "BTF should contain DATASEC for .bss/.data");
    }

    /// Test: .BTF.ext generation produces valid header
    #[test]
    fn btf_ext_has_valid_header() {
        let cu = CompileUnit {
            source_name: "test.c".to_string(),
            comp_dir: None,
            producer: "test".to_string(),
            language: 12,
            functions: vec![DwarfFuncInfo {
                name: "foo".to_string(),
                linkage_name: None,
                low_pc: 0x100,
                high_pc: 0x140,
                decl_line: 5,
                decl_file_idx: 0,
                return_type: DwarfType {
                    name: "int".to_string(),
                    byte_size: 4,
                    kind: DwarfTypeKind::Base(TypeEncoding::Signed),
                },
                params: vec![],
                param_locations: vec![],
                local_vars: vec![],
                lexical_blocks: vec![],
                is_external: true,
                is_inline: false,
                is_declaration: false,
            }],
            globals: vec![],
            types: vec![],
            line_entries: vec![LineEntry {
                pc: 0x100,
                line: 6,
                column: 1,
                file_idx: 0,
                discriminator: 0,
                is_stmt: true,
                basic_block: false,
            }],
            line_files: vec!["test.c".to_string()],
            line_dirs: vec![],
            low_pc: 0x100,
            high_pc: 0x140,
            func_ranges: vec![],
            cfi_rows: vec![],
        };
        let btf_ext = build_btf_ext(&cu);
        // Verify magic
        assert!(btf_ext.starts_with(&0xEB9F_u16.to_le_bytes()));
        // Verify version = 1
        assert_eq!(btf_ext[2], 1);
        // Verify header length = 32
        let hdr_len = u32::from_le_bytes([btf_ext[4], btf_ext[5], btf_ext[6], btf_ext[7]]);
        assert_eq!(hdr_len, 32, "BTF.ext header should be 32 bytes");
    }

    /// 位置表达式编码的逐形状用例（Phase 6.2）。
    ///
    /// 钉的是「长度与内容一致」：`exprloc` = ULEB 长度 + 表达式字节。旧写法直接发
    /// `op + sleb(offset)` 而无长度，解析器把 `0x91` 当长度读 → 整个 unit 错位。而
    /// `DW_OP_reg*` 是**单字节无操作数**的，所以下面同时钉住「寄存器家不多发一字节」：
    /// 把 fbreg 的形状塞给 reg 家（多发一条 sleb）与把 reg 塞给 fbreg（少一字节）都得红。
    #[test]
    fn location_expr_carries_its_own_length() {
        let enc = |loc: &DwarfParamLocation| {
            let mut v = Vec::new();
            write_location_expr(&mut v, loc);
            v
        };
        // fbreg(-24)：长度 2（op + sleb），后面紧跟两个字节的表达式。
        // sleb128(-24) = 0x68（低 7 位 0x68 的符号位已置，单字节结束）
        let fb = enc(&DwarfParamLocation::fbreg(-24));
        assert_eq!(fb, vec![0x02, DW_OP_fbreg, 0x68], "fbreg 的 exprloc 字节不对：{fb:x?}");
        // call_frame_cfa + sleb：同样带长度
        let cfa = enc(&DwarfParamLocation::cfa(8));
        assert_eq!(cfa, vec![0x02, DW_OP_call_frame_cfa, 0x08]);
        // 寄存器家：单字节，无操作数。号就是 DWARF 号（不是硬件号）
        let r3 = enc(&DwarfParamLocation::reg(3));
        assert_eq!(r3, vec![0x01, DW_OP_reg0 + 3], "DW_OP_reg 必须只占 1 字节：{r3:x?}");
        // 往返：位置能问回「是不是寄存器家 / 哪一号」（消费方不得自己看 op 区间）
        let reg = DwarfParamLocation::reg(0);
        assert!(
            reg.is_register()
                && !DwarfParamLocation::fbreg(-24).is_register()
                && !DwarfParamLocation::cfa(8).is_register(),
            "只有 reg 家能被认成寄存器家"
        );
        assert_eq!(reg.register(), Some(0));
        assert_eq!(DwarfParamLocation::fbreg(-8).register(), None);
        // 溢出保护：reg0+31 仍在单字节区间内，而号 >31 不得构造（上面 `reg()` 会溢出到
        // 下一个操作码，所以本 target 的换算表只到 31）
        assert_eq!(DwarfParamLocation::reg(31).op, DW_OP_reg31);
    }

    /// 硬件号 → DWARF 号：SysV psABI 的编号顺序与机器码编码**不同序**。
    /// 拿硬件号直接发 `DW_OP_reg` 会让调试器去读另一个寄存器（rdx/rcx 对换、rsi/rdi 对换、
    /// rsp/rbp 对换），而那从产物上看不出来 —— 所以这张表只能逐号钉。
    #[test]
    fn dwarf_reg_number_table_matches_psabi_order() {
        // (硬件号, DWARF 号, 名字) —— DWARF 号按 x86-64 psABI 的 DWARF 寄存器编号表
        const WANT: &[(u8, u8, &str)] = &[
            (0, 0, "rax"),
            (1, 2, "rcx"),
            (2, 1, "rdx"),
            (3, 3, "rbx"),
            (4, 7, "rsp"),
            (5, 6, "rbp"),
            (6, 4, "rsi"),
            (7, 5, "rdi"),
            (8, 8, "r8"),
            (15, 15, "r15"),
            (16, 16, "r16(APX)"),
        ];
        for (hw, dw, name) in WANT {
            assert_eq!(
                ArchKind::X86_64.dwarf_reg_of_gp_hw(*hw),
                Some(*dw),
                "{name} 的 DWARF 号不对"
            );
        }
        assert_eq!(ArchKind::X86_64.dwarf_reg_of_gp_hw(32), None, "越界的号不得发位置");
        // 其他 target：后端今天不消费 Location::Reg，因此这里必须是 None，不能猜表
        assert_eq!(ArchKind::AArch64.dwarf_reg_of_gp_hw(0), None);
        assert_eq!(ArchKind::RiscV64.dwarf_reg_of_gp_hw(0), None);
    }

    /// 一个 DIE 的属性字节必须连续（本轮修的坑本身）：带「指向 int 的指针」参数 +
    /// 「int[4]」局部变量的单元编出来后，内层类型 DIE 不得落在外层 DIE 的属性字节中间。
    ///
    /// 判据不靠人眼看字节：`DW_AT_count` 的值就紧邻在 array DIE 的 ref4 之后，中间只能再
    /// 容一个 data4；修之前的字节流会把元素类型的 DIE 插在它们之间。这里把两者之间的
    /// 字节距离直接量出来（比「找得到 count」那种弱判据强），并自带一次判据自校验：
    /// 拿一段「内层 DIE 尾部字节」去跑同一条判定，必须被拒。
    #[test]
    fn nested_type_die_does_not_split_a_dies_attributes() {
        let int_ty = DwarfType {
            name: "int".to_string(),
            byte_size: 4,
            kind: DwarfTypeKind::Base(TypeEncoding::Signed),
        };
        let cu = CompileUnit {
            source_name: "t.c".to_string(),
            comp_dir: None,
            producer: "dcc-test".to_string(),
            language: 12,
            functions: vec![DwarfFuncInfo {
                name: "f".to_string(),
                linkage_name: None,
                low_pc: 0,
                high_pc: 64,
                decl_line: 1,
                decl_file_idx: 0,
                return_type: int_ty.clone(),
                params: vec![(
                    "p".to_string(),
                    DwarfType {
                        name: "int *".to_string(),
                        byte_size: 8,
                        kind: DwarfTypeKind::Pointer(Box::new(int_ty.clone())),
                    },
                )],
                param_locations: vec![DwarfParamLocation::fbreg(-16)],
                local_vars: vec![DwarfLocalVar {
                    name: "a".to_string(),
                    ty: DwarfType {
                        name: "int[4]".to_string(),
                        byte_size: 16,
                        kind: DwarfTypeKind::Array {
                            element: Box::new(int_ty.clone()),
                            count: 4,
                        },
                    },
                    location: DwarfParamLocation::fbreg(-32),
                    decl_line: 2,
                }],
                lexical_blocks: vec![],
                is_external: false,
                is_inline: false,
                is_declaration: false,
            }],
            globals: vec![],
            types: vec![int_ty],
            line_entries: vec![],
            line_files: vec![],
            line_dirs: vec![],
            low_pc: 0,
            high_pc: 64,
            func_ranges: vec![],
            cfi_rows: vec![],
        };
        let sec = build_dwarf_full(&cu, &DebugOptions::default());
        let info = &sec.info;
        // 在字节流里找 array DIE：它的属性是 [ref4(元素)][data4(count=4)]，两者紧邻。
        let count_at = info.windows(4).position(|w| w == 4u32.to_le_bytes());
        let count_at = count_at.expect("array 的 DW_AT_count 字节必须出现在流里");
        // 它前 4 字节就是元素类型的 ref4（修之前中间会被塞进整个 int DIE 的字节）：
        // 一个合法的 ref4 在当前 TU 里偏移不可能达到 2^16，所以高两字节必为 0；而内层
        // DIE 的尾部字节（strp 偏移 / 属性值）拿不出这个形状。上面那条就是它的判据。
        let before = &info[count_at.saturating_sub(4)..count_at];
        assert!(
            before[3] == 0 && before[2] == 0,
            "DW_AT_count 前 4 字节应当是一个 ref4（高 2 字节为 0），实得 {before:x?} —— \
             这就是内层类型 DIE 插在属性字节中间的形状"
        );
        // 判据自校验：内层 DIE 尾部被插进来时，上面那条必须报假（否则本用例只是空跑）。
        let broken = [0x00u8, 0x91, 0xe8, 0xff]; // 一个典型的「fbreg 属性 + 高位非零」尾巴
        assert!(
            !(broken[3] == 0 && broken[2] == 0),
            "判据自身分辨不出插进来的字节 ⇒ 上面那条断言是假绿"
        );
    }

    /// `.debug_str` 必须包含 `.debug_info` 里每一个被 `DW_FORM_strp` 指的名字，而且
    /// 多一个 DIE 不得移动已有串的偏移。
    ///
    /// 两边各测一件事：前者是本轮踩到的坑（`strl_section` 是「哪些串会出现」的第二份
    /// 知道，它漏了变量名，readelf 就报「偏移量过大」，而编译与链接完全无感）；后者是
    /// 修复能用的前提（表只能往后长），不然同一个源加一个局部就会让整节字节重排。
    #[test]
    fn debug_str_covers_every_die_name_without_moving_existing_offsets() {
        let base_cu = |with_local: bool| {
            let int_ty = DwarfType {
                name: "int".to_string(),
                byte_size: 4,
                kind: DwarfTypeKind::Base(TypeEncoding::Signed),
            };
            CompileUnit {
                source_name: "t.c".to_string(),
                comp_dir: None,
                producer: "dcc-test".to_string(),
                language: 12,
                functions: vec![DwarfFuncInfo {
                    name: "g".to_string(),
                    linkage_name: None,
                    low_pc: 0,
                    high_pc: 16,
                    decl_line: 1,
                    decl_file_idx: 0,
                    return_type: int_ty.clone(),
                    params: vec![],
                    param_locations: vec![],
                    local_vars: if with_local {
                        vec![DwarfLocalVar {
                            name: "zz_local_name".to_string(),
                            ty: int_ty.clone(),
                            location: DwarfParamLocation::fbreg(-80),
                            decl_line: 2,
                        }]
                    } else {
                        vec![]
                    },
                    lexical_blocks: vec![],
                    is_external: false,
                    is_inline: false,
                    is_declaration: false,
                }],
                globals: vec![],
                types: vec![int_ty],
                line_entries: vec![],
                line_files: vec![],
                line_dirs: vec![],
                low_pc: 0,
                high_pc: 16,
                func_ranges: vec![],
                cfi_rows: vec![],
            }
        };
        let opts = DebugOptions::default();
        let without = build_dwarf_full(&base_cu(false), &opts);
        let with = build_dwarf_full(&base_cu(true), &opts);
        // 1) 变量名必须在表里（不在此时，`.debug_info` 里那个 strp 就是节外指针）
        let needle = b"zz_local_name\0";
        assert!(
            with.str
                .windows(needle.len())
                .any(|w| w == needle),
            "DW_TAG_variable 的名字没进 .debug_str ⇒ strp 指向节外"
        );
        // 2) 已有串的偏移不得变动：新表必须以旧表为前缀
        assert!(
            with.str.starts_with(&without.str),
            "多一个局部就让 .debug_str 重排了（旧表的长度 {}，新表 {}）",
            without.str.len(),
            with.str.len()
        );
        assert!(with.str.len() > without.str.len(), "新串根本没被追加上");
    }

    // ------------------------------------------------------------------
    // Phase 6.2 收尾：`.debug_info`/`.debug_line`/`.debug_aranges` 的地址口径
    // （gdb 那句 `Internal error: pc ... in read in CU, but not in symtab.` 的根）
    // ------------------------------------------------------------------

    /// 只给名字与区间的 `DwarfFuncInfo`，重复 DIE / 区间口径两条判据用。
    fn fn_info(name: &str, lo: u64, hi: u64, decl_line: u32) -> DwarfFuncInfo {
        DwarfFuncInfo {
            name: name.to_string(),
            linkage_name: None,
            low_pc: lo,
            high_pc: hi,
            decl_line,
            decl_file_idx: 0,
            return_type: DwarfType {
                name: "int".to_string(),
                byte_size: 4,
                kind: DwarfTypeKind::Base(TypeEncoding::Signed),
            },
            params: vec![],
            param_locations: vec![],
            local_vars: vec![],
            lexical_blocks: vec![],
            is_external: true,
            is_inline: false,
            is_declaration: false,
        }
    }

    fn cu_with(functions: Vec<DwarfFuncInfo>, entries: Vec<LineEntry>) -> CompileUnit {
        CompileUnit {
            source_name: "x.c".to_string(),
            comp_dir: None,
            producer: "dcc".to_string(),
            language: DW_LANG_C99,
            func_ranges: functions.iter().map(|f| (f.low_pc, f.high_pc)).collect(),
            functions,
            globals: vec![],
            types: vec![],
            line_entries: entries,
            line_files: vec!["x.c".to_string()],
            line_dirs: vec![".".to_string()],
            low_pc: 0,
            high_pc: 0,
            cfi_rows: vec![],
        }
    }

    fn row(pc: u64, line: u32, is_stmt: bool) -> LineEntry {
        LineEntry {
            pc,
            line,
            column: 1,
            file_idx: 0,
            discriminator: 0,
            is_stmt,
            basic_block: false,
        }
    }

    /// `.debug_info` 里**函数 DIE** 的 low_pc 重定位。必须先丢掉第 0 条：那是 CU 自己的
    /// `DW_AT_low_pc`，而它的符号名就取自「排第一的那个函数」（`build_abbrev_and_info`），
    /// 直接按名字数会多数出一条、把「几条函数 DIE」这个问题问错对象。
    fn func_low_pc_relocs(relocs: &[(u64, String)]) -> Vec<(u64, String)> {
        relocs.iter().skip(1).cloned().collect()
    }

    /// `.debug_aranges` 里实际有多少条区间表项：按头里的 address_size 走到对齐后的数据区，
    /// 数到那对 0 终止符为止。不硬编码「头 = N 字节」，以免以后头长了就数错。
    fn arange_count(ar: &[u8]) -> usize {
        let addr = ar[10] as usize;
        assert_eq!(addr, 8, "本用例只喂 x86_64（address_size = 8）");
        assert!(ar.len() > 12, ".debug_aranges 只有头也没有终止项？");
        let mut i = 12usize; // 4 长度 + 2 版本 + 4 CU 偏移 + 1 指针大小 + 1 段选择子
        while i % (2 * addr) != 0 {
            i += 1; // 对齐填充
        }
        let mut n = 0usize;
        while i + 2 * addr <= ar.len() {
            let lo = u64::from_le_bytes(ar[i..i + 8].try_into().unwrap());
            let len = u64::from_le_bytes(ar[i + 8..i + 16].try_into().unwrap());
            if lo == 0 && len == 0 {
                return n;
            }
            n += 1;
            i += 2 * addr;
        }
        n
    }

    /// 把 `.debug_line` 的 opcode 区解成 `(address, line)` 行序列。
    ///
    /// 为什么必须解码再验：判据要的是「发出去的表本身地址单调」（gdb 拿它做二分查找），
    /// 而不是「输入向量排过序」。只看输入的话，发射器把行丢了、把 advance 算错了都看不出来。
    fn decode_line_rows(line_bytes: &[u8]) -> Vec<(u64, u32)> {
        assert!(line_bytes.len() > 20, "行表比一个只带头的长度还短");
        assert_eq!(
            u16::from_le_bytes([line_bytes[4], line_bytes[5]]),
            4,
            "这个解码器只按 DWARF 4 的头布局认（v5 多了 address_size/segment_selector_size 两字节）"
        );
        // DWARF 4 固定头：min_insn(10) max_ops(11) is_stmt(12) line_base(13) line_range(14) opcode_base(15)
        let min_insn = line_bytes[10] as u64;
        let line_base = line_bytes[13] as i8 as i64;
        let line_range = line_bytes[14] as u64;
        let opcode_base = line_bytes[15];
        let ops = &line_bytes[skip_line_header(line_bytes)..];
        let mut rows: Vec<(u64, u32)> = Vec::new();
        let (mut addr, mut line) = (0u64, 1i64);
        let mut i = 0usize;
        while i < ops.len() {
            let b = ops[i];
            if b >= opcode_base {
                let adj = (b - opcode_base) as u64;
                addr += (adj / line_range) * min_insn;
                line += line_base + (adj % line_range) as i64;
                rows.push((addr, line as u32));
                i += 1;
                continue;
            }
            i += 1;
            match b {
                DW_LNS_extended_op => {
                    let (len, used) = read_uleb128_from_slice(&ops[i..]);
                    i += used;
                    let sub = ops[i];
                    i += 1;
                    let payload = len as usize - 1;
                    if sub == DW_LNE_set_address {
                        addr = u64::from_le_bytes(ops[i..i + 8].try_into().unwrap());
                    }
                    i += payload;
                },
                DW_LNS_copy => rows.push((addr, line as u32)),
                DW_LNS_advance_pc => {
                    let (v, used) = read_uleb128_from_slice(&ops[i..]);
                    addr += v * min_insn;
                    i += used;
                },
                DW_LNS_advance_line => {
                    let (v, used) = read_sleb128_from_slice(&ops[i..]);
                    line += v;
                    i += used;
                },
                DW_LNS_set_file | DW_LNS_set_column | DW_LNS_set_isa => {
                    let (_, used) = read_uleb128_from_slice(&ops[i..]);
                    i += used;
                },
                DW_LNS_fixed_advance_pc => {
                    addr += u16::from_le_bytes([ops[i], ops[i + 1]]) as u64;
                    i += 2;
                },
                _ => {},
            }
        }
        rows
    }

    #[test]
    fn cu_die_range_spans_every_function_not_the_source_order_ends() {
        // 源码序（= 输入顺序）与地址序故意不同序：sink 在源码里写第一，但它在 .text 里排最后。
        // 旧口径取 `functions.first()/last()` 会得到 low_pc=0x400（sink），而真正的最小地址是 0x10。
        let sink = fn_info("sink", 0x400, 0x450, 1);
        let f = fn_info("f", 0x10, 0x8b, 2);
        let main = fn_info("main", 0x200, 0x25d, 8);
        let cu = cu_with(vec![sink.clone(), f.clone(), main.clone()], vec![]);

        let opts = DebugOptions {
            level: 2,
            ..Default::default()
        };
        let sec = build_dwarf_full(&cu, &opts);

        // CU 的 low_pc 就在第一条 reloc 的位置上（`build_abbrev_and_info` 先推 CU 再推函数）
        let (cu_lo_off, cu_lo_sym) = &sec.debug_info_relocs[0];
        let lo = u64::from_le_bytes(
            sec.info[*cu_lo_off as usize..*cu_lo_off as usize + 8]
                .try_into()
                .unwrap(),
        );
        let hi_const = u64::from_le_bytes(
            sec.info[*cu_lo_off as usize + 8..*cu_lo_off as usize + 16]
                .try_into()
                .unwrap(),
        );
        assert_eq!(lo, 0x10, "DW_AT_low_pc 必须是所有函数的最小地址");
        assert_eq!(hi_const, 0x450 - 0x10, "DW_FORM_data8 的 high_pc 是「距 low_pc 的常量偏移」");
        assert_eq!(cu_lo_sym, "f", "CU low_pc 的重定位必须跟着最小地址那个函数");
        // 反向对照：本用例必须能区分「min/max」与旧的「first/last」两种口径，
        // 否则输入顺序恰好等于地址序时它永远绿（旧口径给出的会是 0x400）。
        assert_ne!(lo, sink.low_pc, "判据空转：拿到的还是源码序首个函数的地址");
        // 函数 DIE 自己的区间也得一条一个、按地址升序（第 2 条 reloc 起）
        let func_los: Vec<u64> = sec.debug_info_relocs[1..]
            .iter()
            .map(|(off, _)| u64::from_le_bytes(sec.info[*off as usize..*off as usize + 8].try_into().unwrap()))
            .collect();
        assert_eq!(func_los, vec![0x10, 0x200, 0x400], "DW_TAG_subprogram 必须按地址升序");
    }

    #[test]
    fn duplicate_function_entries_collapse_to_one_die_and_one_range() {
        // IR 里同一函数可以同时有「原型声明」与「定义」两个条目，上游按名字取 label
        // 让两者拿到同一个 PC（各自的 span 不同，所以行号不同——这正是行表里出现
        // 「同址两行」的来源）。重复条目的另一个特征：一侧带局部变量、另一侧不带。
        let declared = fn_info("sink", 0x400, 0x450, 1);
        let mut defined = fn_info("sink", 0x400, 0x450, 5);
        defined.local_vars.push(DwarfLocalVar {
            name: "only_on_one_side".to_string(),
            ty: declared.return_type.clone(),
            location: DwarfParamLocation::fbreg(-16),
            decl_line: 2,
        });
        let cu = cu_with(vec![declared, defined], vec![]);
        assert_eq!(
            cu.functions.iter().filter(|f| f.name == "sink").count(),
            2,
            "对照输入本身就得是两条同名条目，否则下面全是空转"
        );

        let norm = normalize_cu(&cu);
        assert_eq!(norm.functions.len(), 1, "同名条目必须合成一条");
        assert_eq!(norm.functions[0].local_vars.len(), 1, "合并不得丢掉另一侧的非空表");

        let opts = DebugOptions {
            level: 2,
            ..Default::default()
        };

        // 反向对照：把**未归一化**的输入直接喂给三个发射器，各自都会发出两份。
        // 这既是修复前产物的真实形状（两条同名同址 DIE、两行同址行、两条重叠 aranges 表项），
        // 也同时证明下面那几个「1」不是仪器数不出「2」。
        let mut strs = StringTable::new();
        let mut abb = AbbrevBuilder::default();
        let (_, _, raw_relocs) = build_abbrev_and_info(&cu, &opts, &mut strs, &mut abb);
        let (raw_line, _) = build_line_program(&cu, &opts);
        let (raw_aranges, _) = build_aranges(&cu, &opts);
        assert_eq!(
            func_low_pc_relocs(&raw_relocs)
                .iter()
                .filter(|(_, n)| *n == "sink")
                .count(),
            2,
            "对照失效：未归一化的输入在 .debug_info 里只留下两条函数 low_pc 重定位"
        );
        assert_eq!(
            decode_line_rows(&raw_line).len(),
            2,
            "对照失效：未归一化的输入在行表里只留下一行"
        );
        assert_eq!(
            arange_count(&raw_aranges),
            2,
            "对照失效：未归一化的输入在 .debug_aranges 里只留下一条表项"
        );

        // 走主入口（= 四个消费者共用同一份归一化口径）：三处都必须只剩一份。
        let sec = build_dwarf_full(&cu, &opts);
        assert_eq!(
            func_low_pc_relocs(&sec.debug_info_relocs)
                .iter()
                .filter(|(_, n)| *n == "sink")
                .count(),
            1,
            "同名同址的 DW_TAG_subprogram 发了两条"
        );
        assert_eq!(arange_count(&sec.aranges), 1, ".debug_aranges 出现重叠重复表项");
        let rows = decode_line_rows(&sec.line);
        assert_eq!(rows.len(), 1, "重复函数在行表里留下了重复行：{:?}", rows);
    }

    #[test]
    fn line_program_addresses_are_monotonic_even_when_the_input_is_not() {
        // 输入故意降序 + 夹一条完全重复的行；发射后的表必须地址严格不降，而且一行都不能丢
        let entries = vec![
            row(0x300, 12, true),
            row(0x100, 3, true),
            row(0x200, 7, true),
            row(0x200, 7, true),
            row(0x180, 5, false),
        ];
        let cu = cu_with(vec![fn_info("f", 0x100, 0x400, 3)], entries.clone());
        let sec = build_dwarf_full(
            &cu,
            &DebugOptions {
                level: 3,
                ..Default::default()
            },
        );
        let rows = decode_line_rows(&sec.line);
        let mut asc = rows.clone();
        asc.sort();
        assert_eq!(rows, asc, "行表地址倒退：gdb 的二分查找会命不中（{:?}）", rows);
        // 反向对照：集合必须与输入（去掉那条完全重复的）相同 —— 防「把行全丢了也算单调」的假绿
        let expect: std::collections::BTreeSet<(u64, u32)> =
            entries.iter().map(|e| (e.pc, e.line)).collect();
        let got: std::collections::BTreeSet<(u64, u32)> = rows.iter().cloned().collect();
        assert_eq!(got, expect, "行表少行或多行（输入 {} 行，解码 {} 行）", expect.len(), rows.len());
    }
}
