// src/codegen/shared/asm_engine.rs
//
// 共享指令引擎: 提供 `.s` 骨架 (段/符号/类型/尺寸/对齐/标签/注释符/字节序),
// 供所有后端在 `-S` / GAS fallback 模式下渲染指令记录。
//
// 边界 (与方案 §4.2 一致):
//   * 词法切分、注释符剥离、行分类一律复用 `asm_source`;
//   * 本模块只做"已经是一行 asm 文本 -> 输出 .s 骨架"与"InsnRec -> 文本"。
//   * 不引入第二套行解析器。

use super::asm_operand::{Arrangement, InsnRec, LayoutMap, Operand, Width};
use super::asm_source::AsmIsa;

/// 某 ISA 的方言描述 (由各后端实现, 本模块只消费)。
#[derive(Clone, Debug)]
pub struct AsmDialect {
    /// 行注释符 (如 aarch64 `//`, x86 `#`)。
    pub comment: &'static str,
    /// 立即数前缀 (如 aarch64 `#`)。
    pub imm_prefix: &'static str,
    /// 字节序: true = 小端, false = 大端。
    pub little_endian: bool,
    /// `.arch` / `.cpu` 声明行 (可选)。
    pub arch_directive: &'static str,
    /// `.syntax` 声明 (如 x86 `att`, 默认空)。
    pub syntax_directive: &'static str,
}

impl AsmDialect {
    pub fn for_isa(isa: AsmIsa) -> Self {
        match isa {
            AsmIsa::AArch64 => AsmDialect {
                comment: "//",
                imm_prefix: "#",
                little_endian: true,
                arch_directive: ".arch armv8-a",
                syntax_directive: "",
            },
            AsmIsa::RiscV64 | AsmIsa::RiscV32 => AsmDialect {
                comment: "#",
                imm_prefix: "",
                little_endian: true,
                arch_directive: ".option norelax",
                syntax_directive: "",
            },
            AsmIsa::Arm => AsmDialect {
                comment: "@",
                imm_prefix: "#",
                little_endian: true,
                arch_directive: ".arch armv7-a",
                syntax_directive: "",
            },
            AsmIsa::Mips32 | AsmIsa::Mips64 => AsmDialect {
                comment: "#",
                imm_prefix: "",
                little_endian: false,
                arch_directive: ".set mips3",
                syntax_directive: "",
            },
            AsmIsa::X86_64 => AsmDialect {
                comment: "#",
                imm_prefix: "$",
                little_endian: true,
                arch_directive: "",
                syntax_directive: ".att_syntax",
            },
        }
    }
}

/// 寄存器名渲染: 按 dialect 把 (idx, width) 转成字符串。
/// 这是最基础的命名表; 后端可以在自己的 dialect 模块里覆盖。
pub fn render_reg_aarch64(idx: u8, w: Width) -> String {
    match w {
        Width::B32 => format!("w{}", idx),
        Width::B64 => format!("x{}", idx),
        Width::B128 => format!("v{}", idx),
        Width::B64 /* d */ => format!("d{}", idx),
        Width::B32 /* s */ => format!("s{}", idx),
        _ => format!("x{}", idx),
    }
}

pub fn render_reg_riscv(idx: u8, _w: Width) -> String {
    // RISC-V ABI 名表 (简化版): x0..x31 用 ABI 名, 后端可覆盖
    const ABI: [&str; 32] = [
        "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3",
        "a4", "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11",
        "t3", "t4", "t5", "t6",
    ];
    ABI.get(idx as usize).map(|s| s.to_string()).unwrap_or_else(|| format!("x{}", idx))
}

/// 渲染单个操作数为 asm 文本。
pub fn render_operand(isa: AsmIsa, op: &Operand) -> String {
    match op {
        Operand::None => String::new(),
        Operand::Reg { idx, width } => match isa {
            AsmIsa::AArch64 => render_reg_aarch64(*idx, *width),
            AsmIsa::RiscV64 | AsmIsa::RiscV32 => render_reg_riscv(*idx, *width),
            AsmIsa::Arm => format!("r{}", idx),
            AsmIsa::Mips32 | AsmIsa::Mips64 => format!("$({})", idx),
            AsmIsa::X86_64 => match width {
                Width::B8 => format!("%al+{}", idx),
                Width::B16 => format!("%ax+{}", idx),
                Width::B32 => format!("%eax+{}", idx),
                Width::B64 => format!("%rax+{}", idx),
                _ => format!("%r{}", idx),
            },
        },
        Operand::Imm { value, .. } => match isa {
            AsmIsa::AArch64 | AsmIsa::Arm => format!("#{}", value),
            AsmIsa::X86_64 => format!("${}", value),
            _ => format!("{}", value),
        },
        Operand::Mem { base, index, scale, disp, writeback } => {
            let mut s = String::new();
            if *disp != 0 {
                s.push_str(&disp.to_string());
            }
            s.push('[');
            if let Some(b) = base {
                s.push_str(&render_operand(isa, &Operand::Reg { idx: *b, width: Width::B64 }));
            }
            if let Some(i) = index {
                s.push_str(", ");
                s.push_str(&render_operand(isa, &Operand::Reg { idx: *i, width: Width::B64 }));
                if *scale != 1 {
                    s.push_str(&format!(", {}", scale));
                }
            }
            s.push(']');
            if *writeback {
                s.push('!');
            }
            s
        }
        Operand::Sym { name, addend } => {
            if *addend != 0 {
                format!("{}{:+}", name, addend)
            } else {
                name.clone()
            }
        }
        Operand::Addr { label, .. } => label.clone(),
        Operand::Vector { idx, arr } => {
            let suffix = match arr {
                Arrangement::B8 => ".b8",
                Arrangement::B16 => ".h",
                Arrangement::B32 => ".s",
                Arrangement::B64 => ".d",
                Arrangement::B128 => ".q",
                Arrangement::Stride => ".stride",
                Arrangement::Multiple => ".multiple",
            };
            format!("v{}{}", idx, suffix)
        }
        Operand::Pad { bytes } => {
            let hex: Vec<String> = bytes.iter().map(|b| format!("0x{:02x}", b)).collect();
            format!(".byte {}", hex.join(", "))
        }
        Operand::PoolData { bytes, alignment } => {
            let words: Vec<String> = bytes
                .chunks(4)
                .map(|c| {
                    let mut v = 0u32;
                    for (i, b) in c.iter().enumerate() {
                        v |= (*b as u32) << (i * 8);
                    }
                    format!("0x{:08x}", v)
                })
                .collect();
            format!(".p2align {}; .word {}", alignment, words.join(", "))
        }
    }
}

/// `.s` 文档头部骨架。
pub fn render_file_header(dialect: &AsmDialect, source_name: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!("{} Generated by dcc (asm_engine)\n", dialect.comment));
    s.push_str(&format!("{} source: {}\n", dialect.comment, source_name));
    if !dialect.arch_directive.is_empty() {
        s.push_str(dialect.arch_directive);
        s.push('\n');
    }
    if !dialect.syntax_directive.is_empty() {
        s.push_str(dialect.syntax_directive);
        s.push('\n');
    }
    s
}

/// 函数序言 (`.type`/`.globl`/`.func`)。
pub fn render_function_prologue(_dialect: &AsmDialect, name: &str, global: bool) -> String {
    let mut s = String::new();
    if global {
        s.push_str(&format!(".globl {}\n", name));
    }
    s.push_str(&format!(".type {}, @function\n", name));
    s.push_str(&format!("{}:\n", name));
    s
}

/// 函数尾声 (`.size`)。
pub fn render_function_epilogue(dialect: &AsmDialect, name: &str, size: u64) -> String {
    let _ = dialect;
    format!(".size {}, {}\n", name, size)
}

/// 渲染一条 `InsnRec` (占位实现: 后端应提供自己的 opcode -> 模板映射)。
/// 这里只做"opcode 编号 + 操作数列表"的兜底输出, 供测试与未迁移的发射点使用。
pub fn render_insn_fallback(isa: AsmIsa, rec: &InsnRec) -> String {
    let ops: Vec<String> = (0..rec.nops as usize)
        .map(|i| render_operand(isa, rec.op(i)))
        .filter(|s| !s.is_empty())
        .collect();
    format!("/* op={} */ {}", rec.opcode, ops.join(", "))
}

/// 把指令序列渲染成 `.s` 文本。
pub fn render_text_section(
    isa: AsmIsa,
    recs: &[InsnRec],
    layout: &LayoutMap,
) -> String {
    let mut s = String::new();
    s.push_str("\n.text\n");
    for rec in recs {
        // 若该指令 pc 对应某个函数起点, 插一个标签
        for (name, addr) in &layout.addresses {
            if *addr == rec.pc && !name.is_empty() && !name.starts_with('.') {
                s.push_str(&format!("{}:\n", name));
            }
        }
        s.push_str("  ");
        s.push_str(&render_insn_fallback(isa, rec));
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_aarch64() {
        let d = AsmDialect::for_isa(AsmIsa::AArch64);
        assert_eq!(d.comment, "//");
        assert!(d.little_endian);
    }

    #[test]
    fn render_imm() {
        assert_eq!(
            render_operand(AsmIsa::AArch64, &Operand::uimm(4, 12)),
            "#4"
        );
        assert_eq!(
            render_operand(AsmIsa::X86_64, &Operand::uimm(4, 32)),
            "$4"
        );
    }
}
