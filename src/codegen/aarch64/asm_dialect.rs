// src/codegen/aarch64/asm_dialect.rs
//
// AArch64 ISA 的方言描述: 注释符、立即数前缀、寄存器命名表、内存操作数归属规则。
// 本模块只提供"发射点构造 InsnRec"与"InsnRec -> GAS 文本"所需的最小静态信息;
// 具体位型编码表与 mini_asm 的 encode_aarch64 保持一致 (迁移期不改语义)。

use crate::codegen::shared::asm_operand::{Arrangement, Operand, Width};
use crate::codegen::shared::asm_source::AsmIsa;

/// AArch64 寄存器命名表: 返回 (name, is_64bit)。
/// 与 mini_asm::parse_aarch64_reg 互逆 (往返测试钉住)。
pub fn reg_name(idx: u8, w: Width) -> String {
    match idx {
        31 => match w {
            Width::B64 => "xzr".to_string(),
            Width::B32 => "wzr".to_string(),
            Width::B128 => "zr".to_string(),
            _ => "xzr".to_string(),
        },
        30 if matches!(w, Width::B64) => "lr".to_string(),
        _ => match w {
            Width::B64 => format!("x{}", idx),
            Width::B32 => format!("w{}", idx),
            Width::B128 => format!("v{}", idx),
            Width::B64 /* d */ => format!("d{}", idx),
            Width::B32 /* s */ => format!("s{}", idx),
            Width::B16 => format!("h{}", idx),
            Width::B8 => format!("b{}", idx),
            _ => format!("x{}", idx),
        },
    }
}

/// 把 (name, is_64bit) 反向解析回 (idx, width)。与 parse_aarch64_reg 对偶。
pub fn parse_reg(name: &str) -> Option<(u8, Width)> {
    let name = name.trim().to_lowercase();
    match name.as_str() {
        "sp" | "xzr" => return Some((31, Width::B64)),
        "wzr" => return Some((31, Width::B32)),
        "lr" => return Some((30, Width::B64)),
        _ => {}
    }
    // 第一个字符决定宽度, 其余是寄存器编号。
    let mut chars = name.chars();
    let prefix_c = chars.next()?;
    let rest = chars.as_str();
    let w = match prefix_c {
        'x' => Width::B64,
        'w' => Width::B32,
        'v' => Width::B128,
        'd' => Width::B64,
        's' => Width::B32,
        'h' => Width::B16,
        'b' => Width::B8,
        _ => return None,
    };
    let idx: u8 = rest.parse().ok()?;
    if idx > 31 {
        return None;
    }
    Some((idx, w))
}

/// 渲染一个操作数为 AArch64 GAS 文本。
pub fn render_operand(op: &Operand) -> String {
    match op {
        Operand::None => String::new(),
        Operand::Reg { idx, width } => reg_name(*idx, *width),
        Operand::Imm { value, .. } => format!("#{}", value),
        Operand::Mem { base, index, scale, disp, writeback } => {
            let mut s = String::new();
            if *disp != 0 {
                s.push_str(&format!("#{}, ", disp));
            }
            s.push('[');
            if let Some(b) = base {
                s.push_str(&reg_name(*b, Width::B64));
            }
            if let Some(i) = index {
                s.push_str(", ");
                s.push_str(&reg_name(*i, Width::B64));
                if *scale != 1 {
                    s.push_str(&format!(", LSL #{}", scale));
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
                Arrangement::B8 => ".8b",
                Arrangement::B16 => ".4h",
                Arrangement::B32 => ".4s",
                Arrangement::B64 => ".2d",
                Arrangement::B128 => ".16b",
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
            let mut s = format!(".p2align {}", alignment);
            for chunk in bytes.chunks(4) {
                let mut v = 0u32;
                for (i, b) in chunk.iter().enumerate() {
                    v |= (*b as u32) << (i * 8);
                }
                s.push_str(&format!("\n.word 0x{:08x}", v));
            }
            s
        }
    }
}

/// 返回该 dialect 对应的 AsmIsa。
pub fn isa() -> AsmIsa {
    AsmIsa::AArch64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reg_name_roundtrip() {
        for idx in 0u8..=30 {
            let n = reg_name(idx, Width::B64);
            let (i, w) = parse_reg(&n).unwrap();
            assert_eq!(i, idx);
            assert_eq!(w, Width::B64);

            let n = reg_name(idx, Width::B32);
            let (i, w) = parse_reg(&n).unwrap();
            assert_eq!(i, idx);
            assert_eq!(w, Width::B32);
        }
        assert_eq!(reg_name(31, Width::B64), "xzr");
        assert_eq!(reg_name(31, Width::B32), "wzr");
        assert_eq!(reg_name(30, Width::B64), "lr");
    }

    #[test]
    fn render_menm_with_disp() {
        let op = Operand::Mem {
            base: Some(9),
            index: None,
            scale: 1,
            disp: 16,
            writeback: false,
        };
        assert_eq!(render_operand(&op), "#16, [x9]");
    }
}
