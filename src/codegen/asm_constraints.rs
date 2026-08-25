// src/codegen/asm_constraints.rs - 非 x86 后端共享 inline asm 约束解析
//
// 为 AArch64 / RISC-V / MIPS / ARM 后端提供:
// - 约束字符串解析 (输出/输入/匹配/多重/inout)
// - 调用者保存寄存器池分配
// - 增强版模板占位符替换 (使用架构实际寄存器名)
// - 架构特定模板修饰符 (%a / %P / %l / %c / %w / %x 等)

// ---------------------------------------------------------------------------
// 架构寄存器定义
// ---------------------------------------------------------------------------

/// 单个可分配寄存器
#[derive(Clone, Debug)]
pub struct AllocReg {
    /// 寄存器编号 (硬件编码)
    pub idx: u8,
    /// 32 位名称 (w0, a0, $t0, r0)
    pub name32: &'static str,
    /// 64 位名称 (x0, a0, $t0, r0) — 与 32 位相同时可复用
    pub name64: &'static str,
}

/// 架构寄存器池
pub struct ArchRegPool {
    /// 调用者保存 (scratch) GP 寄存器, 按分配优先级排列
    pub scratch_gp: Vec<AllocReg>,
}

/// AArch64 调用者保存: x0–x15 (x0 返回值, x1–x7 参数, x8 间接结果)
pub fn aarch64_reg_pool() -> ArchRegPool {
    let regs: Vec<AllocReg> = (0..=15u8)
        .map(|i| AllocReg {
            idx: i,
            name32: aarch64_w_name(i),
            name64: aarch64_x_name(i),
        })
        .collect();
    ArchRegPool { scratch_gp: regs }
}

/// RISC-V 调用者保存: a0–a7 (x10–x17), t0–t6 (x5–x7, x28–x31)
pub fn riscv64_reg_pool() -> ArchRegPool {
    let mut regs = Vec::new();
    // a0–a7
    for i in 0..8u8 {
        regs.push(AllocReg {
            idx: 10 + i,
            name32: riscv_abi_name(10 + i),
            name64: riscv_abi_name(10 + i),
        });
    }
    // t0–t6
    for &x in &[5u8, 6, 7, 28, 29, 30, 31] {
        regs.push(AllocReg {
            idx: x,
            name32: riscv_abi_name(x),
            name64: riscv_abi_name(x),
        });
    }
    ArchRegPool { scratch_gp: regs }
}

/// MIPS O32 调用者保存: $t0–$t9 ($8–$15, $24–$25), $v0/$v1 ($2–$3)
pub fn mips_reg_pool() -> ArchRegPool {
    let mut regs = Vec::new();
    // $v0, $v1
    for &x in &[2u8, 3] {
        regs.push(AllocReg {
            idx: x,
            name32: mips_reg_name(x),
            name64: mips_reg_name(x),
        });
    }
    // $t0–$t7
    for i in 0..8u8 {
        regs.push(AllocReg {
            idx: 8 + i,
            name32: mips_reg_name(8 + i),
            name64: mips_reg_name(8 + i),
        });
    }
    // $t8, $t9
    for &x in &[24u8, 25] {
        regs.push(AllocReg {
            idx: x,
            name32: mips_reg_name(x),
            name64: mips_reg_name(x),
        });
    }
    ArchRegPool { scratch_gp: regs }
}

/// ARM AAPCS 调用者保存: r0–r3 (参数/返回值), r12 (ip)
pub fn arm_reg_pool() -> ArchRegPool {
    let regs: Vec<AllocReg> = [0u8, 1, 2, 3, 12]
        .iter()
        .map(|&i| AllocReg {
            idx: i,
            name32: arm_reg_name(i),
            name64: arm_reg_name(i),
        })
        .collect();
    ArchRegPool { scratch_gp: regs }
}

// ---------------------------------------------------------------------------
// 寄存器名称辅助
// ---------------------------------------------------------------------------

fn aarch64_x_name(i: u8) -> &'static str {
    match i {
        0 => "x0", 1 => "x1", 2 => "x2", 3 => "x3", 4 => "x4", 5 => "x5",
        6 => "x6", 7 => "x7", 8 => "x8", 9 => "x9", 10 => "x10", 11 => "x11",
        12 => "x12", 13 => "x13", 14 => "x14", 15 => "x15", 16 => "x16",
        17 => "x17", 18 => "x18", 19 => "x19", 20 => "x20", 21 => "x21",
        22 => "x22", 23 => "x23", 24 => "x24", 25 => "x25", 26 => "x26",
        27 => "x27", 28 => "x28", 29 => "x29", 30 => "x30", 31 => "sp",
        _ => "xzr",
    }
}

fn aarch64_w_name(i: u8) -> &'static str {
    match i {
        0 => "w0", 1 => "w1", 2 => "w2", 3 => "w3", 4 => "w4", 5 => "w5",
        6 => "w6", 7 => "w7", 8 => "w8", 9 => "w9", 10 => "w10", 11 => "w11",
        12 => "w12", 13 => "w13", 14 => "w14", 15 => "w15", 16 => "w16",
        17 => "w17", 18 => "w18", 19 => "w19", 20 => "w20", 21 => "w21",
        22 => "w22", 23 => "w23", 24 => "w24", 25 => "w25", 26 => "w26",
        27 => "w27", 28 => "w28", 29 => "w29", 30 => "w30", 31 => "wsp",
        _ => "wzr",
    }
}

fn riscv_abi_name(x: u8) -> &'static str {
    match x {
        0 => "zero", 1 => "ra", 2 => "sp", 3 => "gp", 4 => "tp",
        5 => "t0", 6 => "t1", 7 => "t2",
        8 => "s0", 9 => "s1",
        10 => "a0", 11 => "a1", 12 => "a2", 13 => "a3",
        14 => "a4", 15 => "a5", 16 => "a6", 17 => "a7",
        18 => "s2", 19 => "s3", 20 => "s4", 21 => "s5",
        22 => "s6", 23 => "s7", 24 => "s8", 25 => "s9",
        26 => "s10", 27 => "s11",
        28 => "t3", 29 => "t4", 30 => "t5", 31 => "t6",
        _ => "??",
    }
}

fn mips_reg_name(r: u8) -> &'static str {
    match r {
        0 => "$zero", 1 => "$at", 2 => "$v0", 3 => "$v1",
        4 => "$a0", 5 => "$a1", 6 => "$a2", 7 => "$a3",
        8 => "$t0", 9 => "$t1", 10 => "$t2", 11 => "$t3",
        12 => "$t4", 13 => "$t5", 14 => "$t6", 15 => "$t7",
        16 => "$s0", 17 => "$s1", 18 => "$s2", 19 => "$s3",
        20 => "$s4", 21 => "$s5", 22 => "$s6", 23 => "$s7",
        24 => "$t8", 25 => "$t9", 26 => "$k0", 27 => "$k1",
        28 => "$gp", 29 => "$sp", 30 => "$fp", 31 => "$ra",
        _ => "$??",
    }
}

fn arm_reg_name(r: u8) -> &'static str {
    match r {
        0 => "r0", 1 => "r1", 2 => "r2", 3 => "r3",
        4 => "r4", 5 => "r5", 6 => "r6", 7 => "r7",
        8 => "r8", 9 => "r9", 10 => "r10", 11 => "fp",
        12 => "ip", 13 => "sp", 14 => "lr", 15 => "pc",
        _ => "r?",
    }
}

// ---------------------------------------------------------------------------
// 约束分类
// ---------------------------------------------------------------------------

/// 约束类型
#[derive(Clone, Debug, PartialEq)]
pub enum ConstraintKind {
    /// `"=r"` 纯输出到寄存器
    OutputReg,
    /// `"=m"` 纯输出到内存
    OutputMem,
    /// `"+r"` 读写寄存器
    InOutReg,
    /// `"+m"` 读写内存
    InOutMem,
    /// `"r"` 输入寄存器
    InputReg,
    /// `"m"` 输入内存
    InputMem,
    /// `"i"` 立即数
    Immediate,
    /// `"0".."9"` 匹配约束 (复用另一操作数寄存器)
    Matched(usize),
    /// `"memory"` clobber — 编译器屏障
    MemoryClobber,
    /// `"cc"` clobber — 条件码
    CcClobber,
    /// 其他 / 未知
    Other,
}

/// 已解析的单个操作数信息
#[derive(Clone, Debug)]
pub struct ResolvedOperand {
    /// 在全部操作数列表中的位置索引 (输出在前, 输入在后)
    pub index: usize,
    /// 约束分类
    pub kind: ConstraintKind,
    /// 分配的寄存器 (如果有)
    pub reg: Option<AllocReg>,
    /// 是否为输出 (含 inout)
    pub is_output: bool,
    /// 是否为输入 (含 inout)
    pub is_input: bool,
    /// 原始约束字符串
    pub constraint: String,
    /// 操作数名称 (如果有)
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// 约束解析
// ---------------------------------------------------------------------------

/// 解析单个约束字符串
pub fn parse_single_constraint(c: &str) -> ConstraintKind {
    let c = c.trim();
    if c.is_empty() {
        return ConstraintKind::Other;
    }
    // memory / cc clobber
    if c == "memory" {
        return ConstraintKind::MemoryClobber;
    }
    if c == "cc" {
        return ConstraintKind::CcClobber;
    }
    // 匹配约束: 纯数字
    if c.chars().all(|ch| ch.is_ascii_digit()) && !c.is_empty() {
        if let Ok(n) = c.parse::<usize>() {
            return ConstraintKind::Matched(n);
        }
    }
    let first = c.chars().next().unwrap();
    match first {
        '=' => {
            let rest = &c[1..];
            match rest.chars().next() {
                Some('r') => ConstraintKind::OutputReg,
                Some('m') => ConstraintKind::OutputMem,
                _ => ConstraintKind::OutputReg, // 默认按寄存器
            }
        }
        '+' => {
            let rest = &c[1..];
            match rest.chars().next() {
                Some('r') => ConstraintKind::InOutReg,
                Some('m') => ConstraintKind::InOutMem,
                _ => ConstraintKind::InOutReg,
            }
        }
        'r' => ConstraintKind::InputReg,
        'm' => ConstraintKind::InputMem,
        'i' => ConstraintKind::Immediate,
        'g' => ConstraintKind::InputReg, // "g" 通用 → 按寄存器处理
        'n' => ConstraintKind::Immediate, // "n" 已知整数常量
        'X' => ConstraintKind::Immediate, // "X" 任意常量
        // 多重约束: 取第一个有效选项
        _ if c.len() > 1 => {
            // 例如 "rm" → 取 'r'
            if c.contains('r') {
                ConstraintKind::InputReg
            } else if c.contains('m') {
                ConstraintKind::InputMem
            } else if c.contains('i') || c.contains('n') {
                ConstraintKind::Immediate
            } else {
                ConstraintKind::Other
            }
        }
        _ => ConstraintKind::Other,
    }
}

/// Check if a constraint string is supported by the compiler.
/// Returns None if supported, or Some(reason) if unsupported.
/// P0-5: early validation rejects unsupported operand patterns
/// before codegen, producing clear error messages.
pub fn validate_constraint(c: &str) -> Option<String> {
    let c = c.trim();
    if c.is_empty() {
        return Some("empty constraint".to_string());
    }
    // Supported clobbers
    if c == "memory" || c == "cc" {
        return None;
    }
    // Matched constraints: pure digits
    if c.chars().all(|ch| ch.is_ascii_digit()) && !c.is_empty() {
        return None;
    }
    let first = c.chars().next().unwrap();
    match first {
        '=' => {
            let rest = &c[1..];
            match rest.chars().next() {
                Some('r') | Some('m') | Some('Q') | Some('R') | Some('S')
                | Some('T') | Some('U') | Some('a') | Some('b') | Some('c')
                | Some('d') | Some('S') | Some('D') => None,
                Some(ch) if ch.is_ascii_alphanumeric() => None,
                _ => None, // default to output reg
            }
        }
        '+' => {
            let rest = &c[1..];
            match rest.chars().next() {
                Some('r') | Some('m') => None,
                Some(ch) if ch.is_ascii_alphanumeric() => None,
                _ => None, // default to inout reg
            }
        }
        // Supported single-letter constraints
        'r' | 'm' | 'i' | 'g' | 'n' | 'X' | 'o' | 'V' | 'p' | 's' |
        'E' | 'F' | 'w' | 'x' | 'y' | 't' | 'u' |
        'a' | 'b' | 'c' | 'd' | 'S' | 'D' => None,
        // Multi-character constraints
        _ if c.len() > 1 => {
            // Check if any known constraint letter is present
            let known = "rmignXoVpsEtwxytabcdSUQ";
            if c.chars().any(|ch| known.contains(ch)) {
                None
            } else {
                Some(format!("unsupported constraint '{}'", c))
            }
        }
        _ => Some(format!("unsupported constraint '{}'", c)),
    }
}

/// Validate all output and input constraints for an inline asm statement.
/// Returns a list of error messages for unsupported constraints.
pub fn validate_asm_constraints(
    output_constraints: &[String],
    input_constraints: &[String],
) -> Vec<String> {
    let mut errors = Vec::new();
    for c in output_constraints {
        if let Some(reason) = validate_constraint(c) {
            errors.push(format!("output constraint '{}': {}", c, reason));
        }
    }
    for c in input_constraints {
        if let Some(reason) = validate_constraint(c) {
            errors.push(format!("input constraint '{}': {}", c, reason));
        }
    }
    errors
}

/// 解析全部操作数约束, 分配寄存器
pub fn resolve_asm_operands(
    output_constraints: &[String],
    input_constraints: &[String],
    output_names: &[Option<String>],
    input_names: &[Option<String>],
    pool: &ArchRegPool,
) -> Vec<ResolvedOperand> {
    let mut result = Vec::new();
    let mut used_regs: Vec<u8> = Vec::new();
    let n_outputs = output_constraints.len();

    // 第一遍: 处理输出
    for (i, c) in output_constraints.iter().enumerate() {
        let kind = parse_single_constraint(c);
        let mut op = ResolvedOperand {
            index: i,
            kind: kind.clone(),
            reg: None,
            is_output: true,
            is_input: matches!(kind, ConstraintKind::InOutReg | ConstraintKind::InOutMem),
            constraint: c.clone(),
            name: output_names.get(i).cloned().flatten(),
        };
        match &kind {
            ConstraintKind::OutputReg | ConstraintKind::InOutReg => {
                if let Some(r) = alloc_reg(pool, &used_regs) {
                    used_regs.push(r.idx);
                    op.reg = Some(r);
                }
            }
            _ => {}
        }
        result.push(op);
    }

    // 第二遍: 处理输入 (含匹配约束)
    for (i, c) in input_constraints.iter().enumerate() {
        let kind = parse_single_constraint(c);
        let mut op = ResolvedOperand {
            index: n_outputs + i,
            kind: kind.clone(),
            reg: None,
            is_output: false,
            is_input: true,
            constraint: c.clone(),
            name: input_names.get(i).cloned().flatten(),
        };
        match &kind {
            ConstraintKind::InputReg | ConstraintKind::InOutReg => {
                if let Some(r) = alloc_reg(pool, &used_regs) {
                    used_regs.push(r.idx);
                    op.reg = Some(r);
                }
            }
            ConstraintKind::Matched(target) => {
                // 复用目标操作数的寄存器
                if *target < result.len() {
                    if let Some(ref r) = result[*target].reg {
                        op.reg = Some(r.clone());
                    }
                }
            }
            _ => {}
        }
        result.push(op);
    }

    result
}

/// 从寄存器池分配一个未使用的寄存器
fn alloc_reg(pool: &ArchRegPool, used: &[u8]) -> Option<AllocReg> {
    pool.scratch_gp
        .iter()
        .find(|r| !used.contains(&r.idx))
        .cloned()
}

// ---------------------------------------------------------------------------
// 增强版模板替换
// ---------------------------------------------------------------------------

/// 架构类型 (用于选择寄存器名格式和修饰符语义)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AsmArch {
    AArch64,
    RiscV64,
    Mips,
    Arm,
}

/// 增强版 asm 模板占位符替换:
/// - `%%` → `%`
/// - `%N` → 实际寄存器名 (按操作数索引)
/// - `%[name]` → 按名称查找后替换为寄存器名
/// - `%lN` / `%l[name]` → asm goto 标签名
/// - 架构特定修饰符:
///   - AArch64: `%wN` → w-reg, `%xN` → x-reg
///   - RISC-V: 无特殊修饰符
///   - MIPS: `%N` → $reg
///   - ARM: 无特殊修饰符
pub fn resolve_asm_template_enhanced(
    asm_str: &str,
    operands: &[ResolvedOperand],
    output_names: &[Option<String>],
    input_names: &[Option<String>],
    goto_labels: &[(String, String)], // (label_name, resolved_name)
    arch: AsmArch,
    use_64bit: bool,
) -> String {
    let s = strip_asm_quotes(asm_str);
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '%' && i + 1 < chars.len() {
            match chars[i + 1] {
                // %% → %
                '%' => {
                    result.push('%');
                    i += 2;
                }
                // %[name] → 按名称查找
                '[' => {
                    if let Some(end) = chars[i + 2..].iter().position(|&c| c == ']') {
                        let name: String = chars[i + 2..i + 2 + end].iter().collect();
                        // 在操作数中查找
                        let all_names: Vec<&Option<String>> =
                            output_names.iter().chain(input_names.iter()).collect();
                        for (idx, n) in all_names.iter().enumerate() {
                            if n.as_ref().map(|s| s.as_str()) == Some(&name) {
                                if let Some(op) = operands.get(idx) {
                                    result.push_str(&format_reg(op, arch, use_64bit, None));
                                } else {
                                    result.push_str(&format!("%{}", idx));
                                }
                                break;
                            }
                        }
                        i += 2 + end + 1;
                    } else {
                        result.push(chars[i]);
                        i += 1;
                    }
                }
                // %l → asm goto 标签
                'l' if i + 2 < chars.len() => {
                    if chars[i + 2] == '[' {
                        // %l[name]
                        if let Some(end) = chars[i + 3..].iter().position(|&c| c == ']') {
                            let name: String = chars[i + 3..i + 3 + end].iter().collect();
                            let resolved = goto_labels
                                .iter()
                                .find(|(n, _)| n == &name)
                                .map(|(_, r)| r.clone())
                                .unwrap_or_else(|| name.clone());
                            result.push_str(&resolved);
                            i += 3 + end + 1;
                        } else {
                            result.push(chars[i]);
                            i += 1;
                        }
                    } else if chars[i + 2].is_ascii_digit() {
                        // %lN
                        let num_start = i + 2;
                        let mut num_end = num_start;
                        while num_end < chars.len() && chars[num_end].is_ascii_digit() {
                            num_end += 1;
                        }
                        let idx_str: String = chars[num_start..num_end].iter().collect();
                        if let Ok(idx) = idx_str.parse::<usize>() {
                            if let Some((_, resolved)) = goto_labels.get(idx) {
                                result.push_str(resolved);
                            } else {
                                result.push_str(&format!(".Lasm_label_{}", idx));
                            }
                        }
                        i = num_end;
                    } else {
                        result.push(chars[i]);
                        i += 1;
                    }
                }
                // 架构特定修饰符
                'w' | 'x' if arch == AsmArch::AArch64 && i + 2 < chars.len() => {
                    let modifier = chars[i + 1];
                    if chars[i + 2] == '[' {
                        // %w[name] / %x[name]
                        if let Some(end) = chars[i + 3..].iter().position(|&c| c == ']') {
                            let name: String = chars[i + 3..i + 3 + end].iter().collect();
                            let all_names: Vec<&Option<String>> =
                                output_names.iter().chain(input_names.iter()).collect();
                            for (idx, n) in all_names.iter().enumerate() {
                                if n.as_ref().map(|s| s.as_str()) == Some(&name) {
                                    if let Some(op) = operands.get(idx) {
                                        let force64 = modifier == 'x';
                                        result
                                            .push_str(&format_reg(op, arch, force64, None));
                                    }
                                    break;
                                }
                            }
                            i += 3 + end + 1;
                        } else {
                            result.push(chars[i]);
                            i += 1;
                        }
                    } else if chars[i + 2].is_ascii_digit() {
                        // %wN / %xN
                        let num_start = i + 2;
                        let mut num_end = num_start;
                        while num_end < chars.len() && chars[num_end].is_ascii_digit() {
                            num_end += 1;
                        }
                        let idx_str: String = chars[num_start..num_end].iter().collect();
                        if let Ok(idx) = idx_str.parse::<usize>() {
                            let force64 = modifier == 'x';
                            if let Some(op) = operands.get(idx) {
                                result.push_str(&format_reg(op, arch, force64, None));
                            } else {
                                result.push_str(&format!("%{}", idx));
                            }
                        }
                        i = num_end;
                    } else {
                        result.push(chars[i]);
                        i += 1;
                    }
                }
                // %cN → 立即数 (去前缀)
                'c' if i + 2 < chars.len() && chars[i + 2].is_ascii_digit() => {
                    let num_start = i + 2;
                    let mut num_end = num_start;
                    while num_end < chars.len() && chars[num_end].is_ascii_digit() {
                        num_end += 1;
                    }
                    let idx_str: String = chars[num_start..num_end].iter().collect();
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if let Some(op) = operands.get(idx) {
                            result.push_str(&format_reg(op, arch, use_64bit, Some('c')));
                        } else {
                            result.push_str(&format!("%{}", idx));
                        }
                    }
                    i = num_end;
                }
                // %N → 寄存器名
                c if c.is_ascii_digit() => {
                    let num_start = i + 1;
                    let mut num_end = num_start;
                    while num_end < chars.len() && chars[num_end].is_ascii_digit() {
                        num_end += 1;
                    }
                    let idx_str: String = chars[num_start..num_end].iter().collect();
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if let Some(op) = operands.get(idx) {
                            result.push_str(&format_reg(op, arch, use_64bit, None));
                        } else {
                            result.push_str(&format!("%{}", idx));
                        }
                    }
                    i = num_end;
                }
                _ => {
                    result.push(chars[i]);
                    i += 1;
                }
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }
    result
}

/// 格式化操作数的寄存器名
fn format_reg(
    op: &ResolvedOperand,
    arch: AsmArch,
    use_64bit: bool,
    modifier: Option<char>,
) -> String {
    match &op.reg {
        Some(r) => {
            let name = if use_64bit { r.name64 } else { r.name32 };
            match modifier {
                Some('c') => {
                    // 立即数形式: 去掉架构前缀
                    match arch {
                        AsmArch::Mips => name.to_string(), // $t0 → $t0
                        _ => name.to_string(),
                    }
                }
                _ => name.to_string(),
            }
        }
        None => {
            // 无寄存器分配, 保持 %N 形式
            format!("%{}", op.index)
        }
    }
}

/// 去除 asm 字符串引号
pub fn strip_asm_quotes(s: &str) -> &str {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_output_reg_constraint() {
        assert_eq!(parse_single_constraint("=r"), ConstraintKind::OutputReg);
    }

    #[test]
    fn parse_input_reg_constraint() {
        assert_eq!(parse_single_constraint("r"), ConstraintKind::InputReg);
    }

    #[test]
    fn parse_inout_reg_constraint() {
        assert_eq!(parse_single_constraint("+r"), ConstraintKind::InOutReg);
    }

    #[test]
    fn parse_matched_constraint() {
        assert_eq!(parse_single_constraint("0"), ConstraintKind::Matched(0));
        assert_eq!(parse_single_constraint("3"), ConstraintKind::Matched(3));
    }

    #[test]
    fn parse_memory_clobber() {
        assert_eq!(parse_single_constraint("memory"), ConstraintKind::MemoryClobber);
    }

    #[test]
    fn parse_cc_clobber() {
        assert_eq!(parse_single_constraint("cc"), ConstraintKind::CcClobber);
    }

    #[test]
    fn parse_multi_constraint() {
        assert_eq!(parse_single_constraint("rm"), ConstraintKind::InputReg);
        assert_eq!(parse_single_constraint("ri"), ConstraintKind::InputReg);
    }

    #[test]
    fn aarch64_reg_pool_alloc() {
        let pool = aarch64_reg_pool();
        assert!(pool.scratch_gp.len() >= 16);
        assert_eq!(pool.scratch_gp[0].name64, "x0");
        assert_eq!(pool.scratch_gp[0].name32, "w0");
    }

    #[test]
    fn riscv64_reg_pool_alloc() {
        let pool = riscv64_reg_pool();
        assert!(pool.scratch_gp.len() >= 15);
        assert_eq!(pool.scratch_gp[0].name64, "a0");
    }

    #[test]
    fn mips_reg_pool_alloc() {
        let pool = mips_reg_pool();
        assert!(pool.scratch_gp.len() >= 12);
        assert_eq!(pool.scratch_gp[0].name32, "$v0");
    }

    #[test]
    fn arm_reg_pool_alloc() {
        let pool = arm_reg_pool();
        assert_eq!(pool.scratch_gp.len(), 5);
        assert_eq!(pool.scratch_gp[0].name32, "r0");
    }

    #[test]
    fn resolve_simple_output_input() {
        let pool = aarch64_reg_pool();
        let ops = resolve_asm_operands(
            &["=r".to_string()],
            &["r".to_string()],
            &[None],
            &[None],
            &pool,
        );
        assert_eq!(ops.len(), 2);
        assert!(ops[0].is_output);
        assert!(ops[0].reg.is_some());
        assert!(ops[1].is_input);
        assert!(ops[1].reg.is_some());
        // 两个操作数应分配不同寄存器
        assert_ne!(ops[0].reg.as_ref().unwrap().idx, ops[1].reg.as_ref().unwrap().idx);
    }

    #[test]
    fn resolve_matched_constraint() {
        let pool = aarch64_reg_pool();
        let ops = resolve_asm_operands(
            &["=r".to_string()],
            &["0".to_string()],
            &[None],
            &[None],
            &pool,
        );
        assert_eq!(ops.len(), 2);
        // 匹配约束应复用 output 的寄存器
        assert_eq!(
            ops[0].reg.as_ref().unwrap().idx,
            ops[1].reg.as_ref().unwrap().idx
        );
    }

    #[test]
    fn template_substitution_basic() {
        let pool = aarch64_reg_pool();
        let ops = resolve_asm_operands(
            &["=r".to_string()],
            &["r".to_string()],
            &[None],
            &[None],
            &pool,
        );
        let result = resolve_asm_template_enhanced(
            "\"mov %0, %1\"",
            &ops,
            &[None],
            &[None],
            &[],
            AsmArch::AArch64,
            false,
        );
        // %0 和 %1 应被替换为实际寄存器名
        assert!(!result.contains("%0"));
        assert!(!result.contains("%1"));
        assert!(result.contains("w")); // w0, w1 等
    }

    #[test]
    fn template_substitution_named() {
        let pool = aarch64_reg_pool();
        let ops = resolve_asm_operands(
            &["=r".to_string()],
            &["r".to_string()],
            &[Some("out".to_string())],
            &[Some("in".to_string())],
            &pool,
        );
        let result = resolve_asm_template_enhanced(
            "\"mov %[out], %[in]\"",
            &ops,
            &[Some("out".to_string())],
            &[Some("in".to_string())],
            &[],
            AsmArch::AArch64,
            false,
        );
        assert!(!result.contains("%[out]"));
        assert!(!result.contains("%[in]"));
    }

    #[test]
    fn template_percent_escape() {
        let ops: Vec<ResolvedOperand> = vec![];
        let result = resolve_asm_template_enhanced(
            "\"100%%\"",
            &ops,
            &[],
            &[],
            &[],
            AsmArch::AArch64,
            false,
        );
        assert!(result.contains("100%"));
    }

    #[test]
    fn strip_asm_quotes_works() {
        assert_eq!(strip_asm_quotes("\"hello\""), "hello");
        assert_eq!(strip_asm_quotes("hello"), "hello");
        assert_eq!(strip_asm_quotes("\"\""), "");
    }

    #[test]
    fn validate_constraint_supported() {
        // Common supported constraints
        assert!(validate_constraint("r").is_none());
        assert!(validate_constraint("m").is_none());
        assert!(validate_constraint("i").is_none());
        assert!(validate_constraint("g").is_none());
        assert!(validate_constraint("n").is_none());
        assert!(validate_constraint("=r").is_none());
        assert!(validate_constraint("=m").is_none());
        assert!(validate_constraint("+r").is_none());
        assert!(validate_constraint("+m").is_none());
        assert!(validate_constraint("memory").is_none());
        assert!(validate_constraint("cc").is_none());
        assert!(validate_constraint("0").is_none());
        assert!(validate_constraint("3").is_none());
        assert!(validate_constraint("rm").is_none());
    }

    #[test]
    fn validate_constraint_unsupported() {
        // Unsupported constraint letters
        assert!(validate_constraint("Z").is_some());
        assert!(validate_constraint("J").is_some());
        assert!(validate_constraint("K").is_some());
        assert!(validate_constraint("L").is_some());
        assert!(validate_constraint("M").is_some());
        assert!(validate_constraint("N").is_some());
        assert!(validate_constraint("O").is_some());
        assert!(validate_constraint("P").is_some());
        assert!(validate_constraint("").is_some());
    }

    #[test]
    fn validate_asm_constraints_batch() {
        let errors = validate_asm_constraints(
            &["=r".to_string(), "ZH".to_string()],
            &["r".to_string(), "WL".to_string()],
        );
        // ZH and WL contain no known constraint letters
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("ZH"));
        assert!(errors[1].contains("WL"));
    }
}
