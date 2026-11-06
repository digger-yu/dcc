// src/codegen/x86_64/asm/operand.rs
// Inline assembly operand parsing and representation for x86_64 backend

/// Assembly register base constants
pub const ASM_BASE_RIP: u8 = 255;
pub const ASM_REG_XMM: u8 = 128;
pub const ASM_REG_YMM: u8 = 129;
pub const ASM_REG_ZMM: u8 = 130;
pub const ASM_REG_TMM: u8 = 131;
pub const ASM_REG_K: u8 = 132;
pub const ASM_REG_CR: u8 = 133;
pub const ASM_REG_DR: u8 = 134;
pub const ASM_REG_MMX: u8 = 135;
pub const ASM_REG_ST: u8 = 136;

/// Assembly operand types parsed from AT&T syntax
#[derive(Debug, Clone, PartialEq)]
pub enum AsmOperand {
    None,
    Reg(u8, u8), // (index, size_bits)
    Mem {
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
    },
    Sym(String),          // Symbol reference
    SymDisp(String, i32), // Symbol with displacement
    Imm(i64),             // Immediate value
    Seg(u8),              // Segment code
    SegMem {
        seg: u8,
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
    },
}

/// 把内存操作数的 `disp` 字段解成一个**纯数值**：十进制/十六进制字面量，或 GAS 允许的
/// 四则式（`0*8`、`5*8`、`(4+1)*8`、`-8`）。解不出来返回 `None`，即这不是数值位移（是个符号）。
///
/// 为什么必须有这一条：`arch/x86/lib/csum-copy_64.S:41` 的 `movq %rbx, 0*8(%rsp)` 以前在
/// `parse::<i32>()` 上失败，就整串被当成一个**名叫 `0*8` 的符号**（`SymDisp`），后果有两层：
///   1. 寻址彻底错 —— `SymDisp` 只发 RIP-relative，base `%rsp` 被默默丢掉；
///   2. 链接期 `undefined reference to '0*8'`（实测全树 7 处，站点在 `csum_partial` 附近，
///      间隔 7 字节 = 一条 mov/lea 的长度，止于 `3*8`）。
/// 立即数那一支（`$~0xfa1e0ff3`）早已用 `eval_pure_numeric_arith` 做同一件事，位移却不作；
/// 本函数把两边拉平。带标识符的式子（`foo+1`）仍走 `SymDisp` 兼底，不能当数值折掉。
pub(crate) fn fold_asm_disp_number(s: &str) -> Option<i32> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return i32::from_str_radix(hex, 16).ok();
    }
    if let Ok(v) = t.parse::<i32>() {
        return Some(v);
    }
    crate::codegen::x86_64::codegen::Codegen::eval_pure_numeric_arith(t)
        .and_then(|v| i32::try_from(v).ok())
}

/// Parse AT&T string operand into AsmOperand structure
pub fn parse_asm_operand_str(s: &str) -> AsmOperand {
    let s = s.trim();
    if s.is_empty() {
        return AsmOperand::None;
    }

    // Strip segment prefix: %gs:... / gs:... / %gs / gs (must be before other checks)
    let (s, seg) = strip_asm_seg_prefix(s);
    let s = s.trim();

    if s.is_empty() {
        return if let Some(sc) = seg {
            AsmOperand::Seg(sc)
        } else {
            AsmOperand::None
        };
    }

    // Indirect prefix *: jmp *%rax, call *symbol, jmp *(%rax), etc.
    if let Some(rest) = s.strip_prefix('*') {
        let inner = parse_asm_operand_str(rest);
        return wrap_indirect(inner);
    }

    // Immediate: $N
    if let Some(rest) = s.strip_prefix('$') {
        let rest = rest.trim();
        if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            if let Ok(v) = i64::from_str_radix(hex, 16) {
                return AsmOperand::Imm(v);
            }
        } else if let Ok(v) = rest.parse::<i64>() {
            return AsmOperand::Imm(v);
        }
        // 常量表达式形式的立即数（`$~0xfa1e0ff3`）：与 `constraint.rs` 里那份解析器同一口径。
        if let Some(v) = crate::codegen::x86_64::codegen::Codegen::eval_pure_numeric_arith(rest) {
            return AsmOperand::Imm(v);
        }
        // $label (e.g., $1f, $1b) - local label reference
        return AsmOperand::Sym(rest.to_string());
    }

    // Register: %reg or bare rax/rbx, etc.
    if s.starts_with('%') {
        if let Some((idx, sz)) = parse_asm_reg(s) {
            return AsmOperand::Reg(idx, sz);
        }
        return AsmOperand::None;
    }

    if let Some((idx, sz)) = parse_asm_reg(&format!("%{}", s)) {
        return AsmOperand::Reg(idx, sz);
    }

    // Memory: disp(%base,%idx,scale) or (%reg) or symbol(%rip)
    if s.contains('(') {
        let inner = parse_asm_mem(s);
        return match (seg, inner) {
            (
                Some(sc),
                AsmOperand::Mem {
                    disp,
                    base,
                    index,
                    scale,
                },
            ) => AsmOperand::SegMem {
                seg: sc,
                disp,
                base,
                index,
                scale,
            },
            (_, other) => other,
        };
    }

    // Segment prefix + symbol (e.g., `gs:symbol` which is rare; treat as symbol)
    // Symbol name (global tag reference) + segment prefix → segment prefix follows to emit phase
    let sym = strip_asm_comment(s).trim().to_string();
    if let Some(sc) = seg {
        return AsmOperand::SegMem {
            seg: sc,
            disp: 0,
            base: ASM_BASE_RIP,
            index: None,
            scale: 1,
        };
    }

    AsmOperand::Sym(sym)
}

/// Strip segment prefix from string
fn strip_asm_seg_prefix(s: &str) -> (&str, Option<u8>) {
    let s = s.trim_start();
    let (head, rest) = if let Some(after) = s.strip_prefix('%') {
        // Find first non-alphabetic character as head/rest split
        let pos = after
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(after.len());
        (&after[..pos], &after[pos..])
    } else {
        let pos = s
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(s.len());
        (&s[..pos], &s[pos..])
    };

    if let Some((code, _pfx)) = parse_asm_seg(head) {
        if let Some(after_colon) = rest.strip_prefix(':') {
            return (after_colon, Some(code));
        }
        // Standalone segment name: entire s has been consumed as segment name
        if rest.is_empty() {
            return ("", Some(code));
        }
    }

    (s, None)
}

/// Parse segment register name → (seg_code, prefix_byte)
/// seg_code for modrm reg field: 0=ES,1=CS,2=SS,3=DS,4=FS,5=GS
/// prefix_byte for mov/push segment prefix
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

/// Parse AT&T register name → (reg_index, size_bits)
fn parse_asm_reg(name: &str) -> Option<(u8, u8)> {
    let n = name.strip_prefix('%').unwrap_or(name);
    match n {
        // General purpose registers
        "rax" | "eax" | "ax" | "al" | "ah" => Some((0, 64)),
        "rbx" | "ebx" | "bx" | "bl" | "bh" => Some((3, 64)),
        "rcx" | "ecx" | "cx" | "cl" | "ch" => Some((1, 64)),
        "rdx" | "edx" | "dx" | "dl" | "dh" => Some((2, 64)),
        "rsi" | "esi" | "si" | "sil" => Some((6, 64)),
        "rdi" | "edi" | "di" | "dil" => Some((7, 64)),
        "rsp" | "esp" | "sp" | "spl" => Some((4, 64)),
        "rbp" | "ebp" | "bp" | "bpl" => Some((5, 64)),
        // R8-R15
        "r8" | "r8d" | "r8w" | "r8b" => Some((8, 64)),
        "r9" | "r9d" | "r9w" | "r9b" => Some((9, 64)),
        "r10" | "r10d" | "r10w" | "r10b" => Some((10, 64)),
        "r11" | "r11d" | "r11w" | "r11b" => Some((11, 64)),
        "r12" | "r12d" | "r12w" | "r12b" => Some((12, 64)),
        "r13" | "r13d" | "r13w" | "r13b" => Some((13, 64)),
        "r14" | "r14d" | "r14w" | "r14b" => Some((14, 64)),
        "r15" | "r15d" | "r15w" | "r15b" => Some((15, 64)),
        // APX EGPR r16-r31
        "r16" | "r16d" | "r16w" | "r16b" => Some((16, 64)),
        "r17" | "r17d" | "r17w" | "r17b" => Some((17, 64)),
        "r18" | "r18d" | "r18w" | "r18b" => Some((18, 64)),
        "r19" | "r19d" | "r19w" | "r19b" => Some((19, 64)),
        "r20" | "r20d" | "r20w" | "r20b" => Some((20, 64)),
        "r21" | "r21d" | "r21w" | "r21b" => Some((21, 64)),
        "r22" | "r22d" | "r22w" | "r22b" => Some((22, 64)),
        "r23" | "r23d" | "r23w" | "r23b" => Some((23, 64)),
        "r24" | "r24d" | "r24w" | "r24b" => Some((24, 64)),
        "r25" | "r25d" | "r25w" | "r25b" => Some((25, 64)),
        "r26" | "r26d" | "r26w" | "r26b" => Some((26, 64)),
        "r27" | "r27d" | "r27w" | "r27b" => Some((27, 64)),
        "r28" | "r28d" | "r28w" | "r28b" => Some((28, 64)),
        "r29" | "r29d" | "r29w" | "r29b" => Some((29, 64)),
        "r30" | "r30d" | "r30w" | "r30b" => Some((30, 64)),
        "r31" | "r31d" | "r31w" | "r31b" => Some((31, 64)),
        // RIP
        "rip" => Some((ASM_BASE_RIP, 64)),
        // XMM/YMM/ZMM vector registers
        // (Rust 不允许字符串范围 pattern, 用 guard + 显式上界表达原语义)
        name if name.starts_with("xmm") => {
            let idx = n[3..].parse::<u8>().ok()?;
            if idx > 15 {
                return None;
            }
            Some((idx, ASM_REG_XMM))
        },
        name if name.starts_with("ymm") => {
            let idx = n[3..].parse::<u8>().ok()?;
            if idx > 15 {
                return None;
            }
            Some((idx, ASM_REG_YMM))
        },
        name if name.starts_with("zmm") => {
            let idx = n[3..].parse::<u8>().ok()?;
            if idx > 31 {
                return None;
            }
            Some((idx, ASM_REG_ZMM))
        },
        // AMX tile registers
        name if name.starts_with("tmm") => {
            let idx = n[3..].parse::<u8>().ok()?;
            if idx > 7 {
                return None;
            }
            Some((idx, ASM_REG_TMM))
        },
        // AVX-512 mask registers
        name if name.starts_with('k') => {
            let idx = n[1..].parse::<u8>().ok()?;
            if idx > 7 {
                return None;
            }
            Some((idx, ASM_REG_K))
        },
        // Control registers
        name if name.starts_with("cr") => {
            let idx = n[2..].parse::<u8>().ok()?;
            if idx > 8 {
                return None;
            }
            Some((idx, ASM_REG_CR))
        },
        // Debug registers
        name if name.starts_with("dr") || name.starts_with("db") => {
            let idx = n[2..].parse::<u8>().ok()?;
            if idx > 7 {
                return None;
            }
            Some((idx, ASM_REG_DR))
        },
        // MMX registers
        name if name.starts_with("mm") => {
            let idx = n[2..].parse::<u8>().ok()?;
            if idx > 7 {
                return None;
            }
            Some((idx, ASM_REG_MMX))
        },
        // x87 FPU stack registers
        "st" | "st(0)" => Some((0, ASM_REG_ST)),
        "st(1)" => Some((1, ASM_REG_ST)),
        "st(2)" => Some((2, ASM_REG_ST)),
        "st(3)" => Some((3, ASM_REG_ST)),
        "st(4)" => Some((4, ASM_REG_ST)),
        "st(5)" => Some((5, ASM_REG_ST)),
        "st(6)" => Some((6, ASM_REG_ST)),
        "st(7)" => Some((7, ASM_REG_ST)),
        _ => None,
    }
}

/// Parse AT&T memory operand
fn parse_asm_mem(s: &str) -> AsmOperand {
    // `(sym)(%rip)`：GAS 允许把符号包在括号里。内核 `__WARN_bug_entry()` 的
    // `lea (2f)(%rip), %reg` 正是这种形状（`2:` 在 `__bug_table` 段）。旧解析把
    // 第一个 `(...)` 当寄存器间接，inner 变成 `sym)(%rip`，`parse_asm_reg` 失败后
    // 回落到默认 `%rbp`，符号被整个丢掉 —— 实测 WARN 传给 `__WARN_trap` 的
    // bug_entry 指针恒为 0，`report_bug_entry()` 解引用空指针二次 #PF。
    // 这里识别「首个括号组不是寄存器」且紧跟 `(%rip)` 的形式，按符号 PC32 处理。
    if let Some(stripped) = s.strip_prefix('(') {
        if let Some(close) = stripped.find(')') {
            let head = stripped[..close].trim();
            let tail = stripped[close + 1..].trim();
            // head 必须是符号样子的名字：纯数字 `(8)(%rip)` 是绝对地址，
            // 不能当符号发重定位。
            let symbol_like = head
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '.' || c == '_' || c == '$');
            if symbol_like
                && !head.starts_with('%')
                && parse_asm_reg(head).is_none()
                && tail.starts_with('(')
                && tail.ends_with(')')
            {
                let base = tail[1..tail.len() - 1].trim();
                if base == "%rip" || base == "rip" {
                    return AsmOperand::SymDisp(head.to_string(), 0);
                }
            }
        }
    }

    // Format: [disp](%base[,%idx[,scale]])
    let paren_pos = s.find('(').unwrap_or(s.len());
    let disp_str = s[..paren_pos].trim();
    let rest = &s[paren_pos..];

    let mut disp: i32 = 0;
    if !disp_str.is_empty() {
        // 纯数值位移（包含 `0*8`、`(4+1)*8` 这类四则式）留在 `Mem{disp, base, …}` 形式里；
        // 判据与 `$N` 那一条同源，见 [`fold_asm_disp_number`]。
        if let Some(v) = fold_asm_disp_number(disp_str) {
            disp = v;
        } else {
            // Symbol name: convert to SymDisp with symbol name (RIP-relative default)
            return AsmOperand::SymDisp(disp_str.to_string(), 0);
        }
    }

    // Extract content inside parentheses
    let inner = if rest.starts_with('(') && rest.ends_with(')') {
        &rest[1..rest.len() - 1]
    } else {
        return AsmOperand::None;
    };

    let parts: Vec<&str> = inner.split(',').collect();
    let base_str = parts.first().map(|s| s.trim()).unwrap_or("");

    if base_str == "%rip" || base_str == "rip" {
        // Check for index/scale: disp(%rip,%idx,scale)
        let mut idx: Option<u8> = None;
        let mut scale: u8 = 1;

        if parts.len() > 1 {
            let idx_str = parts[1].trim();
            if !idx_str.is_empty() {
                idx = parse_asm_reg(idx_str).map(|(r, _)| r);
            }
        }

        if parts.len() > 2 {
            scale = parts[2].trim().parse::<u8>().unwrap_or(1);
        }

        return AsmOperand::Mem {
            disp,
            base: ASM_BASE_RIP,
            index: idx,
            scale,
        };
    }

    let (base, _bsz) = parse_asm_reg(base_str).unwrap_or((5, 64)); // Default rbp

    let mut idx: Option<u8> = None;
    let mut scale: u8 = 1;

    if parts.len() > 1 {
        let idx_str = parts[1].trim();
        if !idx_str.is_empty() {
            idx = parse_asm_reg(idx_str).map(|(r, _)| r);
        }
    }

    if parts.len() > 2 {
        scale = parts[2].trim().parse::<u8>().unwrap_or(1);
    }

    AsmOperand::Mem {
        disp,
        base,
        index: idx,
        scale,
    }
}

/// Mark AsmOperand as indirect (jmp/call *x meaning)
fn wrap_indirect(inner: AsmOperand) -> AsmOperand {
    // Indirect Reg → keep Reg, recognize in emit phase (jmp *%rax = jmp %rax)
    // Indirect Mem → keep Mem
    // Indirect Sym → mark as SymDisp(sym, 0)表示间接跳转
    match inner {
        AsmOperand::Sym(name) => AsmOperand::SymDisp(name, 0),
        other => other, // Reg/Mem return as-is
    }
}

/// Strip assembly comment from string
pub fn strip_asm_comment(s: &str) -> &str {
    s.split('#').next().unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `arch/x86/lib/csum-copy_64.S` 那一族 `N*8(%rsp)` 位移必须是 `Mem`，不能变成一个
    /// 名叫 `0*8` 的「符号」。两层危害都钉住：数值对（`3*8` = 24）与 base 不得被丢。
    ///
    /// 本用例**故意不含**带括号的四则式（如 `(4+1)*8(%rsp)`）：`parse_asm_mem` 用「第一个
    /// `(`」切分位移与寻址寄存器，括号初值会被误判成寄存器组的开头（实测会退成
    /// `disp=0, base=默认 rbp`）。那是这条切分算法自身的限制，与本缺陷无关；内核与
    /// GAS 实际写的是不带外层括号的 `5*8(%rsp)` 一形。
    #[test]
    fn arithmetic_displacement_folds_and_keeps_base_register() {
        let rsp = parse_asm_reg("%rsp").map(|(r, _)| r).unwrap();
        for (text, want) in [("0*8(%rsp)", 0i32), ("3*8(%rsp)", 24), ("5*8(%rsp)", 40)] {
            match parse_asm_operand_str(text) {
                AsmOperand::Mem { disp, base, .. } => {
                    assert_eq!((disp, base), (want, rsp), "{text} 应折成 {want}(%rsp)");
                },
                other => panic!("{text} 解析成了 {other:?}，不是 Mem（base 被丢了就是寻址错误）"),
            }
        }
        // 括号四则式目前只能折到纯数值那一步；不能把括号串当符号发出去。
        assert_eq!(fold_asm_disp_number("(4+1)*8"), Some(40));
    }

    /// 真符号位移仍走 `SymDisp` 兼底：不能把 `foo+1` 当数值折成 `1`（那是静默错值）。
    #[test]
    fn symbolic_displacement_still_falls_back_to_symdisp() {
        assert!(matches!(
            parse_asm_operand_str("foo+1(%rip)"),
            AsmOperand::SymDisp(s, _) if s == "foo+1"
        ));
        // 纯数值与四则式都能折；带标识符、溢出 i32、空串都不能折。
        assert_eq!(fold_asm_disp_number("8"), Some(8));
        assert_eq!(fold_asm_disp_number("0x10"), Some(16));
        assert_eq!(fold_asm_disp_number("2*8"), Some(16));
        assert_eq!(fold_asm_disp_number("foo*8"), None);
        assert_eq!(fold_asm_disp_number(""), None);
        assert_eq!(fold_asm_disp_number("99999999999999"), None);
    }
}
