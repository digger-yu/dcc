// src/codegen/mini_asm.rs - Non-x86 mini-assembler encoding tables
//
// Provides instruction machine-code encoding for AArch64, RISC-V, ARM,
// MIPS and MIPS64 inline assembly. This module is now purely the ISA
// encoding tables: line lexing/classification, the pass-through decision
// and diagnostics all live in `asm_source` (single source of truth for
// every backend, including x86_64 via its injected encoder).

/// Parse AArch64 register name to register number.
/// Supports: x0-x30, w0-w30, sp/xzr/wzr, x30=lr
pub fn parse_aarch64_reg(name: &str) -> Option<(u8, bool)> {
    // (reg_num, is_64bit)
    let name = name.trim().to_lowercase();
    match name.as_str() {
        "sp" | "xzr" => Some((31, true)),
        "wzr" => Some((31, false)),
        "lr" => Some((30, true)),
        _ => {
            if let Some(rest) = name.strip_prefix('x') {
                rest.parse::<u8>().ok().map(|r| (r, true))
            } else if let Some(rest) = name.strip_prefix('w') {
                rest.parse::<u8>().ok().map(|r| (r, false))
            } else {
                None
            }
        },
    }
}

/// Encode AArch64 NOP
pub fn encode_aarch64_nop() -> [u8; 4] {
    [0x1f, 0x20, 0x03, 0xd5]
}

// ===== NEON/SVE register helpers =====

/// Check if operand is a NEON vector register (v/d/s/h/b prefix).
fn is_neon_reg(s: &str) -> bool {
    let s = s.trim().to_lowercase();
    // Strip arrangement specifier like .16b, .4s, .2d
    let s = s.split('.').next().unwrap_or(&s);
    (s.starts_with('v')
        || s.starts_with('d')
        || s.starts_with('s')
        || s.starts_with('h')
        || s.starts_with('b'))
        && s[1..].parse::<u8>().is_ok()
}

/// Parse NEON register name -> (reg_num, size_bits).
/// v=128, d=64, s=32, h=16, b=8
fn parse_neon_reg(s: &str) -> Option<(u8, u16)> {
    let s = s.trim().to_lowercase();
    // Strip arrangement specifier
    let base = s.split('.').next().unwrap_or(&s);
    let (prefix, num_str) = base.split_at(1);
    let num: u8 = num_str.parse().ok()?;
    if num > 31 {
        return None;
    }
    let size = match prefix {
        "v" => 128,
        "d" => 64,
        "s" => 32,
        "h" => 16,
        "b" => 8,
        _ => return None,
    };
    Some((num, size))
}

/// Check if operand is a SVE Z register (z0-z31).
fn is_sve_reg(s: &str) -> bool {
    let s = s.trim().to_lowercase();
    let s = s.split('.').next().unwrap_or(&s);
    s.starts_with('z') && s[1..].parse::<u8>().is_ok()
}

/// Parse SVE Z register name -> reg_num (0-31).
fn parse_sve_reg(s: &str) -> Option<u8> {
    let s = s.trim().to_lowercase();
    let s = s.split('.').next().unwrap_or(&s);
    let num_str = s.strip_prefix('z')?;
    let num: u8 = num_str.parse().ok()?;
    if num > 31 {
        None
    } else {
        Some(num)
    }
}

/// Check if operand is a SVE P register (p0-p15).
fn is_sve_pred_reg(s: &str) -> bool {
    let s = s.trim().to_lowercase();
    let s = s.split('.').next().unwrap_or(&s);
    s.starts_with('p') && s[1..].parse::<u8>().is_ok()
}

/// Parse SVE P register name -> reg_num (0-15).
fn parse_sve_pred_reg(s: &str) -> Option<u8> {
    let s = s.trim().to_lowercase();
    let s = s.split('.').next().unwrap_or(&s);
    let num_str = s.strip_prefix('p')?;
    let num: u8 = num_str.parse().ok()?;
    if num > 15 {
        None
    } else {
        Some(num)
    }
}

// ===== 操作数取用的严格语义 (单一真源) =====================================
//
// 历史缺陷: 各 encode_* 里大量写 `operands.first().and_then(parse).unwrap_or(0)`,
// 把两种完全不同的情况抹平成同一个 0:
//   (a) 操作数省略了   (b) 操作数存在但解析不出来
// 于是 `blr xN` 静默编成 `blr x0`、`ld1w z0.s, [x3]` 静默丢掉基址寄存器 ——
// 这是最难排查的一类 silent miscompile, 也正是「同一源码两模式产出不同机器码」
// 的根因之一。此处把两者分开:
//   req_* : 必需操作数。缺失或解析不出 -> None, 整条编码失败 (交 asm_source 判透传)
//   opt_* : 可省略操作数。缺失 -> 默认值; 存在但解析不出 -> None

/// AArch64 寄存器操作数 (返回 (编号, 是否 64 位))。
fn a64_reg_at(ops: &[String], i: usize) -> Option<(u8, bool)> {
    parse_aarch64_reg(ops.get(i)?.trim())
}
fn req_a64_reg(ops: &[String], i: usize) -> Option<u8> {
    a64_reg_at(ops, i).map(|(n, _)| n)
}
fn opt_a64_reg(ops: &[String], i: usize, dflt: u8) -> Option<u8> {
    if ops.get(i).is_some() {
        req_a64_reg(ops, i)
    } else {
        Some(dflt)
    }
}

/// 立即数词法的单一真源: `#` 可选, 支持 0x/0X 十六进制与负号。
/// 统一为一个 i64 核心, 再由位宽视图去约束 —— 避免同一份词法被写两遍。
fn parse_imm_core(s: &str) -> Option<i64> {
    let s = s.trim().trim_start_matches('#').trim();
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    if digits.is_empty() {
        return None;
    }
    let v = match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(h) => {
            if h.is_empty() {
                return None;
            }
            i64::from_str_radix(h, 16).ok()?
        },
        None => digits.parse::<i64>().ok()?,
    };
    Some(if neg { -v } else { v })
}

/// i32 视图 (RISC-V / MIPS 的立即数域可负)。
fn parse_imm_i32(s: &str) -> Option<i32> {
    Some(parse_imm_core(s)?.try_into().ok()?)
}

/// u32 视图 (AArch64 / ARM 的无符号立即数域)。非数字或负数一律 None。
fn parse_imm_scalar(s: &str) -> Option<u32> {
    Some(parse_imm_core(s)?.try_into().ok()?)
}
fn req_imm(ops: &[String], i: usize) -> Option<u32> {
    parse_imm_scalar(ops.get(i)?)
}
fn opt_imm(ops: &[String], i: usize, dflt: u32) -> Option<u32> {
    if ops.get(i).is_some() {
        req_imm(ops, i)
    } else {
        Some(dflt)
    }
}

/// SVE / SVE 谓词寄存器取用: 同样区分缺位与非法。
fn req_sve_reg(ops: &[String], i: usize) -> Option<u8> {
    parse_sve_reg(ops.get(i)?)
}
fn req_sve_pred_reg(ops: &[String], i: usize) -> Option<u8> {
    parse_sve_pred_reg(ops.get(i)?)
}
/// NEON 寄存器取用: (编号, 元素宽度位)。解析不出绝不默认 v0/128。
fn req_neon_reg(ops: &[String], i: usize) -> Option<(u8, u16)> {
    parse_neon_reg(ops.get(i)?)
}

/// RISC-V / ARM / MIPS 的同构包装。
fn req_riscv_reg(ops: &[String], i: usize) -> Option<u8> {
    parse_riscv_reg(ops.get(i)?.trim())
}
fn opt_riscv_reg(ops: &[String], i: usize, dflt: u8) -> Option<u8> {
    if ops.get(i).is_some() {
        req_riscv_reg(ops, i)
    } else {
        Some(dflt)
    }
}
fn req_mips_imm(ops: &[String], i: usize) -> Option<i32> {
    parse_mips_imm(ops.get(i)?)
}
fn opt_mips_imm(ops: &[String], i: usize, dflt: i32) -> Option<i32> {
    if ops.get(i).is_some() {
        req_mips_imm(ops, i)
    } else {
        Some(dflt)
    }
}

/// Encode AArch64 instruction from mnemonic + operands.
/// Returns encoded bytes (little-endian, 4 bytes per instruction).
pub fn encode_aarch64(mnemonic: &str, operands: &[String]) -> Option<Vec<u8>> {
    // Note: NEON/SVE instruction matching is done via operand type
    // detection (is_neon_reg/is_sve_reg) in the match arms below.
    match mnemonic {
        "nop" => Some(encode_aarch64_nop().to_vec()),

        // DMB variants
        "dmb" => {
            let option = if operands.is_empty() {
                0xb // ISH default
            } else {
                match operands[0].to_lowercase().as_str() {
                    "ish" => 0xb,
                    "ishst" => 0xa,
                    "ishld" => 0x9,
                    "osh" => 0x7,
                    "oshst" => 0x6,
                    "oshld" => 0x5,
                    "nsh" => 0x3,
                    "nshst" => 0x2,
                    "nshld" => 0x1,
                    "sy" => 0xf,
                    "st" => 0xe,
                    "ld" => 0xd,
                    _ => 0xb,
                }
            };
            // DMB: base = D50330BF, option at bits [11:8]
            let insn: u32 = 0xd50330bf | ((option as u32 & 0xf) << 8);
            Some(insn.to_le_bytes().to_vec())
        },

        // DSB variants
        "dsb" => {
            let option = if operands.is_empty() {
                0xb
            } else {
                match operands[0].to_lowercase().as_str() {
                    "ish" => 0xb,
                    "ishst" => 0xa,
                    "ishld" => 0x9,
                    "osh" => 0x7,
                    "oshst" => 0x6,
                    "oshld" => 0x5,
                    "nsh" => 0x3,
                    "nshst" => 0x2,
                    "nshld" => 0x1,
                    "sy" => 0xf,
                    "st" => 0xe,
                    "ld" => 0xd,
                    _ => 0xb,
                }
            };
            // DSB: base = D503309F, option at bits [11:8]
            let insn: u32 = 0xd503309f | ((option as u32 & 0xf) << 8);
            Some(insn.to_le_bytes().to_vec())
        },

        // ISB
        "isb" => {
            let insn: u32 = 0xd5033fdf;
            Some(insn.to_le_bytes().to_vec())
        },

        // RET {Rn}
        "ret" => {
            let rn = if operands.is_empty() {
                30 // default: x30 (LR)
            } else {
                parse_aarch64_reg(&operands[0])
                    .map(|(r, _)| r)
                    .unwrap_or(30)
            };
            let insn: u32 = 0xd65f0000 | ((rn as u32 & 0x1f) << 5);
            Some(insn.to_le_bytes().to_vec())
        },

        // SVC #imm
        "svc" => {
            let imm = req_imm(operands, 0)?;
            let insn: u32 = 0xd4000001 | ((imm & 0xffff) << 5);
            Some(insn.to_le_bytes().to_vec())
        },

        // BR Xn
        "br" => {
            let rn = req_a64_reg(operands, 0)?;
            let insn: u32 = 0xd61f0000 | ((rn as u32 & 0x1f) << 5);
            Some(insn.to_le_bytes().to_vec())
        },

        // BLR Xn
        "blr" => {
            let rn = req_a64_reg(operands, 0)?;
            let insn: u32 = 0xd63f0000 | ((rn as u32 & 0x1f) << 5);
            Some(insn.to_le_bytes().to_vec())
        },

        // MOV Wd, Wm / MOV Xd, Xm (alias for ORR Rd, RZR, Rm)
        "mov" if operands.len() == 2 => {
            if let (Some((rd, is64)), Some((rm, _))) =
                (parse_aarch64_reg(&operands[0]), parse_aarch64_reg(&operands[1]))
            {
                if is64 {
                    // ORR Xd, XZR, Xm: sf=1, 0a 01010 000 11111 rm rd
                    let insn: u32 = 0xaa0003e0 | ((rm as u32 & 0x1f) << 16) | (rd as u32 & 0x1f);
                    Some(insn.to_le_bytes().to_vec())
                } else {
                    // ORR Wd, WZR, Xm
                    let insn: u32 = 0x2a0003e0 | ((rm as u32 & 0x1f) << 16) | (rd as u32 & 0x1f);
                    Some(insn.to_le_bytes().to_vec())
                }
            } else {
                None
            }
        },

        // MOVZ Wd, #imm / MOVZ Xd, #imm
        "movz" if operands.len() == 2 => {
            if let Some((rd, is64)) = a64_reg_at(operands, 0) {
                let imm = req_imm(operands, 1)?;
                let sf = if is64 { 1u32 << 31 } else { 0 };
                let insn: u32 = sf | 0x52800000 | ((imm & 0xffff) << 5) | (rd as u32 & 0x1f);
                Some(insn.to_le_bytes().to_vec())
            } else {
                None
            }
        },

        // ADD Wd, Wn, Wm / ADD Xd, Xn, Xm (GP only; NEON/SVE vector ADD handled below)
        "add" if operands.len() == 3 && !is_neon_reg(&operands[0]) && !is_sve_reg(&operands[0]) => {
            let rd_res = parse_aarch64_reg(&operands[0]);
            let rn_res = parse_aarch64_reg(&operands[1]);
            let rm_res = parse_aarch64_reg(&operands[2]);
            if let (Some((rd, is64)), Some((rn, _)), Some((rm, _))) = (rd_res, rn_res, rm_res) {
                let sf = if is64 { 1u32 << 31 } else { 0 };
                // ADD: sf 0001011 shift 0 Rm Rn Rd
                let insn: u32 = sf
                    | 0x0b000000
                    | ((rm as u32 & 0x1f) << 16)
                    | ((rn as u32 & 0x1f) << 5)
                    | (rd as u32 & 0x1f);
                Some(insn.to_le_bytes().to_vec())
            } else {
                None
            }
        },

        // SUB Wd, Wn, Wm / SUB Xd, Xn, Xm (GP only; NEON vector SUB handled below)
        "sub" if operands.len() == 3 && !is_neon_reg(&operands[0]) => {
            let rd_res = parse_aarch64_reg(&operands[0]);
            let rn_res = parse_aarch64_reg(&operands[1]);
            let rm_res = parse_aarch64_reg(&operands[2]);
            if let (Some((rd, is64)), Some((rn, _)), Some((rm, _))) = (rd_res, rn_res, rm_res) {
                let sf = if is64 { 1u32 << 31 } else { 0 };
                let insn: u32 = sf
                    | 0x4b000000
                    | ((rm as u32 & 0x1f) << 16)
                    | ((rn as u32 & 0x1f) << 5)
                    | (rd as u32 & 0x1f);
                Some(insn.to_le_bytes().to_vec())
            } else {
                None
            }
        },

        // MSR/MRS 系统寄存器访问: 不做静默 NOP 兜底。
        // 系统寄存器编码依赖完整 SysReg 名表 (op0/op1/CRn/CRm/op2), 属于 GAS 的
        // 既有能力范围; 这里返回 None 让 asm_source 把它标记为"透传给 GAS",
        // 由 TU 级降级机制保证语义正确而非默默丢指令 (P0-5)。

        // CLREX
        "clrex" => {
            let insn: u32 = 0xd5033f5f;
            Some(insn.to_le_bytes().to_vec())
        },

        // SEV / SEVL / WFE / WFI / YIELD
        "sev" => Some(0xd503209fu32.to_le_bytes().to_vec()),
        "sevl" => Some(0xd50320bfu32.to_le_bytes().to_vec()),
        "wfe" => Some(0xd503205fu32.to_le_bytes().to_vec()),
        "wfi" => Some(0xd503207fu32.to_le_bytes().to_vec()),
        "yield" => Some(0xd503203fu32.to_le_bytes().to_vec()),

        // DBG #imm
        "dbg" => {
            let imm = req_imm(operands, 0)?;
            let insn: u32 = 0xd503201f | ((imm & 0xf) << 5);
            Some(insn.to_le_bytes().to_vec())
        },

        // ===== NEON SIMD instructions =====

        // NEON register helper: parse v/d/s/h/b register
        // Returns (reg_num, element_size_bits)
        // v=128, d=64, s=32, h=16, b=8

        // FADD (vector) Vd.T, Vn.T, Vm.T
        // Q=1 for 128-bit, size=00 for .2s, 01 for .4h, 10 for .2d
        "fadd" if operands.len() == 3 && is_neon_reg(&operands[0]) => {
            let (rd, rsz) = req_neon_reg(operands, 0)?;
            let (rn, _) = req_neon_reg(operands, 1)?;
            let (rm, _) = req_neon_reg(operands, 2)?;
            let q = if rsz == 128 { 1u32 << 30 } else { 0 };
            // FADD vector: 0x0E20D400 | Q | size<<22 | Rm | Rn | Rd
            let size = match rsz {
                128 => 0b10u32, // .2d for 128-bit
                64 => 0b10u32,  // .2d
                32 => 0b00u32,  // .2s
                _ => 0b00u32,
            };
            let insn: u32 = 0x0e20d400
                | q
                | ((size & 0x3) << 22)
                | ((rm as u32 & 0x1f) << 16)
                | ((rn as u32 & 0x1f) << 5)
                | (rd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // FSUB (vector) Vd.T, Vn.T, Vm.T
        "fsub" if operands.len() == 3 && is_neon_reg(&operands[0]) => {
            let (rd, rsz) = req_neon_reg(operands, 0)?;
            let (rn, _) = req_neon_reg(operands, 1)?;
            let (rm, _) = req_neon_reg(operands, 2)?;
            let q = if rsz == 128 { 1u32 << 30 } else { 0 };
            let size = match rsz {
                128 => 0b10u32,
                64 => 0b10u32,
                32 => 0b00u32,
                _ => 0b00u32,
            };
            // FSUB vector: 0x2EA0D400 | Q | size<<22 | Rm | Rn | Rd
            let insn: u32 = 0x2ea0d400
                | q
                | ((size & 0x3) << 22)
                | ((rm as u32 & 0x1f) << 16)
                | ((rn as u32 & 0x1f) << 5)
                | (rd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // FMUL (vector) Vd.T, Vn.T, Vm.T
        "fmul" if operands.len() == 3 && is_neon_reg(&operands[0]) => {
            let (rd, rsz) = req_neon_reg(operands, 0)?;
            let (rn, _) = req_neon_reg(operands, 1)?;
            let (rm, _) = req_neon_reg(operands, 2)?;
            let q = if rsz == 128 { 1u32 << 30 } else { 0 };
            let size = match rsz {
                128 => 0b10u32,
                64 => 0b10u32,
                32 => 0b00u32,
                _ => 0b00u32,
            };
            // FMUL vector: 0x2E20DC00 | Q | size<<22 | Rm | Rn | Rd
            let insn: u32 = 0x2e20dc00
                | q
                | ((size & 0x3) << 22)
                | ((rm as u32 & 0x1f) << 16)
                | ((rn as u32 & 0x1f) << 5)
                | (rd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // ADD (vector) Vd.T, Vn.T, Vm.T
        "add" if operands.len() == 3 && is_neon_reg(&operands[0]) => {
            let (rd, rsz) = req_neon_reg(operands, 0)?;
            let (rn, _) = req_neon_reg(operands, 1)?;
            let (rm, _) = req_neon_reg(operands, 2)?;
            let q = if rsz == 128 { 1u32 << 30 } else { 0 };
            // ADD vector: 0x0E208400 | Q | Rm | Rn | Rd (size=00 for .16b/.8b)
            let insn: u32 = 0x0e208400
                | q
                | ((rm as u32 & 0x1f) << 16)
                | ((rn as u32 & 0x1f) << 5)
                | (rd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // SUB (vector) Vd.T, Vn.T, Vm.T
        "sub" if operands.len() == 3 && is_neon_reg(&operands[0]) => {
            let (rd, rsz) = req_neon_reg(operands, 0)?;
            let (rn, _) = req_neon_reg(operands, 1)?;
            let (rm, _) = req_neon_reg(operands, 2)?;
            let q = if rsz == 128 { 1u32 << 30 } else { 0 };
            // SUB vector: 0x2E208400 | Q | Rm | Rn | Rd (size=00 for .16b/.8b)
            let insn: u32 = 0x2e208400
                | q
                | ((rm as u32 & 0x1f) << 16)
                | ((rn as u32 & 0x1f) << 5)
                | (rd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // MUL (vector) Vd.T, Vn.T, Vm.T
        "mul" if operands.len() == 3 && is_neon_reg(&operands[0]) => {
            let (rd, rsz) = req_neon_reg(operands, 0)?;
            let (rn, _) = req_neon_reg(operands, 1)?;
            let (rm, _) = req_neon_reg(operands, 2)?;
            let q = if rsz == 128 { 1u32 << 30 } else { 0 };
            // MUL vector: 0x0E209C00 | Q | Rm | Rn | Rd (size=00 for .16b/.8b)
            let insn: u32 = 0x0e209c00
                | q
                | ((rm as u32 & 0x1f) << 16)
                | ((rn as u32 & 0x1f) << 5)
                | (rd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // LD1 {Vt.T}, [Xn] - load 128-bit vector
        "ld1" if operands.len() == 2 && is_neon_reg(&operands[0]) => {
            let (vt, _) = req_neon_reg(operands, 0)?;
            let (xn, _) = a64_reg_at(operands, 1)?;
            // LD1 (single structure, 128-bit): 0x4C407000 | Rn | Vt
            // objdump 真值 (vt=0, xn=1): 4c407020, 基址骨架正确。
            let insn: u32 = 0x4c407000 | ((xn as u32 & 0x1f) << 5) | (vt as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // ST1 {Vt.T}, [Xn] - store 128-bit vector
        "st1" if operands.len() == 2 && is_neon_reg(&operands[0]) => {
            let (vt, _) = req_neon_reg(operands, 0)?;
            let (xn, _) = a64_reg_at(operands, 1)?;
            // ST1 (single structure, 128-bit): 0x4C007000 | Rn | Vt
            // bit12 (length=1 单结构) 与 LD1 一致, 仅 bit22 (load=1/store=0) 翻转。
            // 旧基址 0x4c006400 把 bit12 错清 0, 被 objdump 证伪。
            let insn: u32 = 0x4c007000 | ((xn as u32 & 0x1f) << 5) | (vt as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // ===== SVE instructions =====

        // PTRUE Pd.T - set all predicate elements to true
        "ptrue" if operands.len() == 1 && is_sve_pred_reg(&operands[0]) => {
            let pd = req_sve_pred_reg(operands, 0)?;
            // PTRUE: 0x25B86000 | Pd
            let insn: u32 = 0x25b86000 | (pd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // WHILELT Pd, Rn, Rm
        "whilelt" if operands.len() == 3 && is_sve_pred_reg(&operands[0]) => {
            let pd = req_sve_pred_reg(operands, 0)?;
            let rn = req_a64_reg(operands, 1)?;
            let rm = req_a64_reg(operands, 2)?;
            // WHILELT: 0x2528A400 | Rn | Rm | Pd
            let insn: u32 = 0x2528a400
                | ((rn as u32 & 0x1f) << 16)
                | ((rm as u32 & 0x1f) << 5)
                | (pd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // SVE FADD Zzd.Pg/M, Zzn, Zzm
        "fadd" if operands.len() == 3 && is_sve_reg(&operands[0]) => {
            let zd = req_sve_reg(operands, 0)?;
            let zn = req_sve_reg(operands, 1)?;
            let zm = req_sve_reg(operands, 2)?;
            // SVE FADD: 0x65000400 | Zm | Zn | Zd (simplified)
            let insn: u32 = 0x65000400
                | ((zm as u32 & 0x1f) << 16)
                | ((zn as u32 & 0x1f) << 5)
                | (zd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // SVE ADD Zzd.Pg/M, Zzn, Zzm
        "add" if operands.len() == 3 && is_sve_reg(&operands[0]) => {
            let zd = req_sve_reg(operands, 0)?;
            let zn = req_sve_reg(operands, 1)?;
            let zm = req_sve_reg(operands, 2)?;
            // SVE ADD: 0x45000400 | Zm | Zn | Zd (simplified)
            let insn: u32 = 0x45000400
                | ((zm as u32 & 0x1f) << 16)
                | ((zn as u32 & 0x1f) << 5)
                | (zd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // LD1W {Zt.Pg}, [Xn] - SVE load vector of words
        "ld1w" if operands.len() == 2 && is_sve_reg(&operands[0]) => {
            let zt = req_sve_reg(operands, 0)?;
            let xn = req_a64_reg(operands, 1)?;
            // LD1W (vector): 0x85800000 | Rn | Zt (simplified)
            let insn: u32 = 0x85800000 | ((xn as u32 & 0x1f) << 5) | (zt as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // ST1W {Zt.Pg}, [Xn] - SVE store vector of words
        "st1w" if operands.len() == 2 && is_sve_reg(&operands[0]) => {
            let zt = req_sve_reg(operands, 0)?;
            let xn = req_a64_reg(operands, 1)?;
            // ST1W (vector): 0xE5800000 | Rn | Zt (simplified)
            let insn: u32 = 0xe5800000 | ((xn as u32 & 0x1f) << 5) | (zt as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        _ => None,
    }
}

// ===== RISC-V register parsing and encoding =====

/// Parse RISC-V register name to register number.
/// Supports both x-names (x0-x31) and ABI names (zero,ra,sp,...).
pub fn parse_riscv_reg(name: &str) -> Option<u8> {
    let name = name.trim().to_lowercase();
    match name.as_str() {
        "zero" => Some(0),
        "ra" => Some(1),
        "sp" => Some(2),
        "gp" => Some(3),
        "tp" => Some(4),
        "t0" => Some(5),
        "t1" => Some(6),
        "t2" => Some(7),
        "s0" | "fp" => Some(8),
        "s1" => Some(9),
        "a0" => Some(10),
        "a1" => Some(11),
        "a2" => Some(12),
        "a3" => Some(13),
        "a4" => Some(14),
        "a5" => Some(15),
        "a6" => Some(16),
        "a7" => Some(17),
        "s2" => Some(18),
        "s3" => Some(19),
        "s4" => Some(20),
        "s5" => Some(21),
        "s6" => Some(22),
        "s7" => Some(23),
        "s8" => Some(24),
        "s9" => Some(25),
        "s10" => Some(26),
        "s11" => Some(27),
        "t3" => Some(28),
        "t4" => Some(29),
        "t5" => Some(30),
        "t6" => Some(31),
        _ => {
            if let Some(rest) = name.strip_prefix('x') {
                rest.parse::<u8>().ok().filter(|&r| r < 32)
            } else {
                None
            }
        },
    }
}

/// Encode RISC-V instruction from mnemonic + operands.
/// Returns encoded bytes (little-endian, 4 bytes per instruction).
pub fn encode_riscv64(mnemonic: &str, operands: &[String]) -> Option<Vec<u8>> {
    match mnemonic {
        "nop" => {
            // ADDI x0, x0, 0
            let insn: u32 = 0x00000013;
            Some(insn.to_le_bytes().to_vec())
        },

        // FENCE pred, succ
        "fence" => {
            let (pred, succ) = if operands.is_empty() {
                (0xfu32, 0xfu32) // RW, RW
            } else if operands.len() == 1 {
                match operands[0].to_lowercase().as_str() {
                    "rw,rw" | "iorw,iorw" => (0xf, 0xf),
                    _ => (0xf, 0xf),
                }
            } else {
                let p = parse_riscv_fence_arg(&operands[0]);
                let s = parse_riscv_fence_arg(&operands[1]);
                (p, s)
            };
            let insn: u32 = 0x0000000f | (pred << 24) | (succ << 20);
            Some(insn.to_le_bytes().to_vec())
        },

        "fence.i" => {
            let insn: u32 = 0x0000100f;
            Some(insn.to_le_bytes().to_vec())
        },

        // EBREAK
        "ebreak" => {
            let insn: u32 = 0x00100073;
            Some(insn.to_le_bytes().to_vec())
        },

        // ECALL
        "ecall" => {
            let insn: u32 = 0x00000073;
            Some(insn.to_le_bytes().to_vec())
        },

        // RET = JALR x0, x1, 0
        "ret" => {
            let insn: u32 = 0x00008067;
            Some(insn.to_le_bytes().to_vec())
        },

        // ADDI rd, rs1, imm
        "addi" if operands.len() == 3 => {
            let rd = parse_riscv_reg(&operands[0])?;
            let rs1 = parse_riscv_reg(&operands[1])?;
            let imm = parse_riscv_imm(&operands[2])?;
            let insn: u32 =
                ((imm as u32 & 0xfff) << 20) | ((rs1 as u32) << 15) | ((rd as u32) << 7) | 0x13;
            Some(insn.to_le_bytes().to_vec())
        },

        // ADD rd, rs1, rs2
        "add" if operands.len() == 3 => {
            let rd = parse_riscv_reg(&operands[0])?;
            let rs1 = parse_riscv_reg(&operands[1])?;
            let rs2 = parse_riscv_reg(&operands[2])?;
            let insn: u32 = ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | ((rd as u32) << 7) | 0x33;
            Some(insn.to_le_bytes().to_vec())
        },

        // SUB rd, rs1, rs2
        "sub" if operands.len() == 3 => {
            let rd = parse_riscv_reg(&operands[0])?;
            let rs1 = parse_riscv_reg(&operands[1])?;
            let rs2 = parse_riscv_reg(&operands[2])?;
            let insn: u32 =
                0x40000033 | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | ((rd as u32) << 7);
            Some(insn.to_le_bytes().to_vec())
        },

        // SFENCE.VMA rs1, rs2  (两个操作数均可省略 = 刷全部)
        "sfence.vma" => {
            let rs1 = opt_riscv_reg(operands, 0, 0)?;
            let rs2 = opt_riscv_reg(operands, 1, 0)?;
            let insn: u32 = 0x12000073 | ((rs1 as u32) << 15) | ((rs2 as u32) << 20);
            Some(insn.to_le_bytes().to_vec())
        },

        // WFI
        "wfi" => {
            let insn: u32 = 0x10500073;
            Some(insn.to_le_bytes().to_vec())
        },

        // PAUSE (Zihintpause, encoded as FENCE tso or ADDI x0,x0,0x010)
        "pause" => {
            let insn: u32 = 0x0100000f; // FENCE tso
            Some(insn.to_le_bytes().to_vec())
        },

        _ => None,
    }
}

fn parse_riscv_fence_arg(s: &str) -> u32 {
    let s = s.trim().to_lowercase();
    let mut val = 0u32;
    for c in s.chars() {
        match c {
            'i' => val |= 8,
            'o' => val |= 4,
            'r' => val |= 2,
            'w' => val |= 1,
            _ => {},
        }
    }
    val
}

fn parse_riscv_imm(s: &str) -> Option<i32> {
    parse_imm_i32(s)
}

// ===== ARM (AArch32) register parsing and encoding =====

/// Parse ARM register name to register number (0-15).
pub fn parse_arm_reg(name: &str) -> Option<u8> {
    let name = name.trim().to_lowercase();
    match name.as_str() {
        "sp" | "r13" => Some(13),
        "lr" | "r14" => Some(14),
        "pc" | "r15" => Some(15),
        "apsr" | "cpsr" | "spsr" => Some(15), // system registers, approximate
        _ => {
            if let Some(rest) = name.strip_prefix('r') {
                rest.parse::<u8>().ok().filter(|&r| r < 16)
            } else {
                None
            }
        },
    }
}

/// Encode ARM instruction from mnemonic + operands.
/// Returns encoded bytes (ARM uses mixed endian, we output little-endian for ARM mode).
pub fn encode_arm(mnemonic: &str, operands: &[String]) -> Option<Vec<u8>> {
    match mnemonic {
        "nop" => {
            // MOV r0, r0 (conditional: AL)
            let insn: u32 = 0xe1a00000;
            Some(insn.to_le_bytes().to_vec())
        },

        // DMB variants (ARMv7+)
        "dmb" => {
            let option = if operands.is_empty() {
                0xb // ISH default
            } else {
                match operands[0].to_lowercase().as_str() {
                    "ish" => 0xb,
                    "ishst" => 0xa,
                    "ishld" => 0x9,
                    "osh" => 0x7,
                    "oshst" => 0x6,
                    "oshld" => 0x5,
                    "nsh" => 0x3,
                    "nshst" => 0x2,
                    "nshld" => 0x1,
                    "sy" => 0xf,
                    "st" => 0xe,
                    "ld" => 0xd,
                    _ => 0xb,
                }
            };
            // DMB: cond=F, 1111 0101 0111 1111 1111 0000 0101 option
            // Encoding: F57FF050 | option
            let insn: u32 = 0xf57ff050 | (option & 0xf);
            Some(insn.to_le_bytes().to_vec())
        },

        // DSB variants
        "dsb" => {
            let option = if operands.is_empty() {
                0xb
            } else {
                match operands[0].to_lowercase().as_str() {
                    "ish" => 0xb,
                    "sy" => 0xf,
                    "st" => 0xe,
                    "ld" => 0xd,
                    _ => 0xb,
                }
            };
            // DSB: F57FF040 | option
            let insn: u32 = 0xf57ff040 | (option & 0xf);
            Some(insn.to_le_bytes().to_vec())
        },

        // ISB
        "isb" => {
            let insn: u32 = 0xf57ff06f;
            Some(insn.to_le_bytes().to_vec())
        },

        // BX Rn
        "bx" => {
            let rn = operands
                .first()
                .and_then(|r| parse_arm_reg(r))
                .unwrap_or(14); // default LR
            let insn: u32 = 0xe12fff10 | (rn as u32 & 0xf);
            Some(insn.to_le_bytes().to_vec())
        },

        // MOV Rd, Rm
        "mov" if operands.len() == 2 => {
            if let (Some(rd), Some(rm)) = (parse_arm_reg(&operands[0]), parse_arm_reg(&operands[1]))
            {
                let insn: u32 = 0xe1a00000 | ((rd as u32 & 0xf) << 12) | (rm as u32 & 0xf);
                Some(insn.to_le_bytes().to_vec())
            } else {
                None
            }
        },

        // ADD Rd, Rn, Rm
        "add" if operands.len() == 3 => {
            if let (Some(rd), Some(rn), Some(rm)) = (
                parse_arm_reg(&operands[0]),
                parse_arm_reg(&operands[1]),
                parse_arm_reg(&operands[2]),
            ) {
                let insn: u32 = 0xe0800000
                    | ((rd as u32 & 0xf) << 12)
                    | ((rn as u32 & 0xf) << 16)
                    | (rm as u32 & 0xf);
                Some(insn.to_le_bytes().to_vec())
            } else {
                None
            }
        },

        // SUB Rd, Rn, Rm
        "sub" if operands.len() == 3 => {
            if let (Some(rd), Some(rn), Some(rm)) = (
                parse_arm_reg(&operands[0]),
                parse_arm_reg(&operands[1]),
                parse_arm_reg(&operands[2]),
            ) {
                let insn: u32 = 0xe0400000
                    | ((rd as u32 & 0xf) << 12)
                    | ((rn as u32 & 0xf) << 16)
                    | (rm as u32 & 0xf);
                Some(insn.to_le_bytes().to_vec())
            } else {
                None
            }
        },

        // SVC / SWI
        "svc" | "swi" => {
            let imm = opt_imm(operands, 0, 0)?;
            let insn: u32 = 0xef000000 | (imm & 0x00ffffff);
            Some(insn.to_le_bytes().to_vec())
        },

        // CLREX
        "clrex" => {
            let insn: u32 = 0xf57ff01f;
            Some(insn.to_le_bytes().to_vec())
        },

        // SEV / SEVL / WFE / WFI / YIELD
        "sev" => Some(0xe320f040u32.to_le_bytes().to_vec()),
        "sevl" => Some(0xe320f060u32.to_le_bytes().to_vec()),
        "wfe" => Some(0xe320f020u32.to_le_bytes().to_vec()),
        "wfi" => Some(0xe320f030u32.to_le_bytes().to_vec()),
        "yield" => Some(0xe320f010u32.to_le_bytes().to_vec()),

        _ => None,
    }
}

// ===== MIPS / MIPS64 register parsing and encoding =====

/// Parse MIPS register name to register number (0-31).
pub fn parse_mips_reg(name: &str) -> Option<u8> {
    let name = name.trim().to_lowercase();
    match name.as_str() {
        "zero" | "$zero" => Some(0),
        "at" | "$at" => Some(1),
        "v0" | "$v0" => Some(2),
        "v1" | "$v1" => Some(3),
        "a0" | "$a0" => Some(4),
        "a1" | "$a1" => Some(5),
        "a2" | "$a2" => Some(6),
        "a3" | "$a3" => Some(7),
        "t0" | "$t0" => Some(8),
        "t1" | "$t1" => Some(9),
        "t2" | "$t2" => Some(10),
        "t3" | "$t3" => Some(11),
        "t4" | "$t4" => Some(12),
        "t5" | "$t5" => Some(13),
        "t6" | "$t6" => Some(14),
        "t7" | "$t7" => Some(15),
        "s0" | "$s0" => Some(16),
        "s1" | "$s1" => Some(17),
        "s2" | "$s2" => Some(18),
        "s3" | "$s3" => Some(19),
        "s4" | "$s4" => Some(20),
        "s5" | "$s5" => Some(21),
        "s6" | "$s6" => Some(22),
        "s7" | "$s7" => Some(23),
        "t8" | "$t8" => Some(24),
        "t9" | "$t9" => Some(25),
        "k0" | "$k0" => Some(26),
        "k1" | "$k1" => Some(27),
        "gp" | "$gp" => Some(28),
        "sp" | "$sp" => Some(29),
        "fp" | "$fp" | "$s8" => Some(30),
        "ra" | "$ra" => Some(31),
        _ => {
            // Try $N form
            let s = name.strip_prefix('$').unwrap_or(&name);
            s.parse::<u8>().ok().filter(|&r| r < 32)
        },
    }
}

fn parse_mips_imm(s: &str) -> Option<i32> {
    parse_imm_i32(s)
}

/// Encode MIPS instruction (works for both MIPS32 and MIPS64).
/// Returns encoded bytes (big-endian, 4 bytes per instruction).
pub fn encode_mips(mnemonic: &str, operands: &[String], is_64bit: bool) -> Option<Vec<u8>> {
    match mnemonic {
        "nop" | "ssnop" => {
            // SLL $0, $0, 0 = 0x00000000
            // ssnop is 0x00000040 for MIPS32r2+
            if mnemonic == "ssnop" {
                Some([0x00, 0x00, 0x00, 0x40].to_vec())
            } else {
                Some([0u8; 4].to_vec())
            }
        },

        // SYNC
        "sync" => {
            // SYNC stype (缺省 0)
            let stype = opt_mips_imm(operands, 0, 0)? as u32;
            let insn: u32 = (stype & 0x1f) << 6 | 0x0f;
            Some(insn.to_be_bytes().to_vec())
        },

        // JR rs
        "jr" => {
            let rs = operands
                .first()
                .and_then(|r| parse_mips_reg(r))
                .unwrap_or(31); // default $ra
            let insn: u32 = ((rs as u32) << 21) | 0x08;
            Some(insn.to_be_bytes().to_vec())
        },

        // JALR rd, rs (or JALR rs => rd=$ra)
        "jalr" => {
            let (rd, rs) = if operands.len() >= 2 {
                (
                    parse_mips_reg(&operands[0]).unwrap_or(31),
                    parse_mips_reg(&operands[1]).unwrap_or(31),
                )
            } else {
                (31, parse_mips_reg(operands.first()?).unwrap_or(31))
            };
            let insn: u32 = ((rs as u32) << 21) | ((rd as u32) << 11) | 0x09;
            Some(insn.to_be_bytes().to_vec())
        },

        // ADDU / OR (for move aliases)
        "move" if operands.len() == 2 => {
            // MOVE rd, rs => OR rd, rs, $0
            let rd = parse_mips_reg(&operands[0])?;
            let rs = parse_mips_reg(&operands[1])?;
            let insn: u32 = ((rs as u32) << 21) | ((rd as u32) << 11) | (rd as u32) | 0x25;
            Some(insn.to_be_bytes().to_vec())
        },

        // ADDU rd, rs, rt
        "addu" if operands.len() == 3 => {
            let rd = parse_mips_reg(&operands[0])?;
            let rs = parse_mips_reg(&operands[1])?;
            let rt = parse_mips_reg(&operands[2])?;
            let insn: u32 = ((rs as u32) << 21) | ((rt as u32) << 16) | ((rd as u32) << 11) | 0x21;
            Some(insn.to_be_bytes().to_vec())
        },

        // ADD rd, rs, rt
        "add" if operands.len() == 3 => {
            let rd = parse_mips_reg(&operands[0])?;
            let rs = parse_mips_reg(&operands[1])?;
            let rt = parse_mips_reg(&operands[2])?;
            let insn: u32 = ((rs as u32) << 21) | ((rt as u32) << 16) | ((rd as u32) << 11) | 0x20;
            Some(insn.to_be_bytes().to_vec())
        },

        // SUB rd, rs, rt
        "sub" if operands.len() == 3 => {
            let rd = parse_mips_reg(&operands[0])?;
            let rs = parse_mips_reg(&operands[1])?;
            let rt = parse_mips_reg(&operands[2])?;
            let insn: u32 = ((rs as u32) << 21) | ((rt as u32) << 16) | ((rd as u32) << 11) | 0x22;
            Some(insn.to_be_bytes().to_vec())
        },

        // SUBU rd, rs, rt
        "subu" if operands.len() == 3 => {
            let rd = parse_mips_reg(&operands[0])?;
            let rs = parse_mips_reg(&operands[1])?;
            let rt = parse_mips_reg(&operands[2])?;
            let insn: u32 = ((rs as u32) << 21) | ((rt as u32) << 16) | ((rd as u32) << 11) | 0x23;
            Some(insn.to_be_bytes().to_vec())
        },

        // DADDU (MIPS64)
        "daddu" if operands.len() == 3 && is_64bit => {
            let rd = parse_mips_reg(&operands[0])?;
            let rs = parse_mips_reg(&operands[1])?;
            let rt = parse_mips_reg(&operands[2])?;
            let insn: u32 = ((rs as u32) << 21) | ((rt as u32) << 16) | ((rd as u32) << 11) | 0x2d;
            Some(insn.to_be_bytes().to_vec())
        },

        // BREAK
        "break" => {
            let code = opt_mips_imm(operands, 0, 0)? as u32;
            let insn: u32 = (code << 6) | 0x0d;
            Some(insn.to_be_bytes().to_vec())
        },

        // SYSCALL
        "syscall" => {
            let code = opt_mips_imm(operands, 0, 0)? as u32;
            let insn: u32 = (code << 6) | 0x0c;
            Some(insn.to_be_bytes().to_vec())
        },

        // CACHE / PREF: 不做静默 NOP 兜底 (需要 op 立即数 + base 寄存器 +
        // 可选索引的复合格式)。返回 None -> asm_source 标记为透传给 GAS。
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== 编码行为差分冻结基线 =====================================
    //
    // mini_asm 的 4 条 match 链即将被改造成「双向编码表」(供 -c 出字节与
    // -S 出文本共用同一份 ISA 知识)。为了让这次改造可证而不是可信,
    // 先用改造前的实现把「哪些输入能编码出什么字节」穷举枚举并冻结成
    // tests/fixtures/asm/mini_asm_encode_freeze.txt; 改造后逐字节比对,
    // 要求 diff = 0 (backend_refactor_plan.md §3.1「行为 diff = 失败」)。
    //
    // 重新生成: cargo test --lib encode_freeze -- --ignored

    /// 各 ISA 的候选助记符 (含已移除的静默 NOP 项, 以钉住它们确实不再产字节)。
    fn freeze_candidates(isa: &str) -> (&'static [&'static str], &'static [&'static str]) {
        match isa {
            "aarch64" => (
                &[
                    "add", "blr", "br", "cache", "clrex", "dbg", "dmb", "dsb", "fadd", "fmul",
                    "fsub", "isb", "ld", "ld1", "ld1w", "mov", "movz", "mrs", "msr", "mul", "nop",
                    "pref", "ptrue", "ret", "sev", "sevl", "st", "st1", "st1w", "sub", "svc",
                    "wfe", "wfi", "whilelt", "yield",
                ],
                &[
                    "x0",
                    "x1",
                    "x2",
                    "x3",
                    "x30",
                    "sp",
                    "xzr",
                    "lr",
                    "w0",
                    "w1",
                    "w15",
                    "wzr",
                    "s0",
                    "s1",
                    "d0",
                    "d1",
                    "v0",
                    "v1",
                    "v0.2d",
                    "v1.2d",
                    "v0.16b",
                    "v0.4s",
                    "z0",
                    "z1",
                    "p0",
                    "p1",
                    "p0.d",
                    "#0",
                    "#1",
                    "#8",
                    "#0xff",
                    "#0x1ff",
                    "#-1",
                    "0",
                    "8",
                    "0xff",
                    "ish",
                    "ishld",
                    "ishst",
                    "osh",
                    "oshld",
                    "oshst",
                    "sy",
                    "[x0]",
                    "[x0, #8]",
                    "[sp, #16]",
                    "[x1, x2]",
                ],
            ),
            "riscv64" => (
                &[
                    "add",
                    "addi",
                    "cache",
                    "ebreak",
                    "ecall",
                    "fence",
                    "fence.i",
                    "msr",
                    "nop",
                    "pause",
                    "ret",
                    "sfence.vma",
                    "sub",
                    "wfi",
                ],
                &[
                    "x0", "x1", "x2", "x10", "x15", "zero", "ra", "sp", "gp", "tp", "t0", "t6",
                    "a0", "a7", "s0", "0", "1", "8", "-1", "0x7ff", "-2048", "1111", "rw", "rw,w",
                    "iorw", "ow",
                ],
            ),
            "arm" => (
                &[
                    "add", "bx", "clrex", "dmb", "dsb", "isb", "ld", "mov", "msr", "nop", "sev",
                    "sevl", "st", "sub", "svc", "swi", "wfe", "wfi", "yield",
                ],
                &[
                    "r0", "r1", "r2", "r3", "r12", "r14", "r15", "sp", "fp", "ip", "lr", "pc",
                    "a0", "a1", "t0", "t3", "s0", "s1", "#0", "#1", "#8", "#0xff", "0", "8",
                    "0xff", "ish", "ishld", "ishst", "osh", "sy", "cc", "eq", "ne",
                ],
            ),
            "mips" => (
                &[
                    "add", "addu", "break", "cache", "daddu", "jalr", "jr", "move", "nop", "pref",
                    "ssnop", "sub", "subu", "sync", "syscall",
                ],
                &[
                    "$0", "$1", "$2", "$4", "$8", "$29", "$31", "zero", "at", "v0", "a0", "t0",
                    "t9", "sp", "ra", "0", "1", "8", "64", "0x40", "$0x7",
                ],
            ),
            _ => (&[], &[]),
        }
    }

    /// 穷举枚举操作数组合, 只保留能成功编码的输入 (即真正的行为面)。
    /// 确定性: 无 HashMap 迭代, 结果排序后输出。
    fn freeze_rows(isa: &str, is_64bit: bool) -> Vec<String> {
        let (mnemonics, pool) = freeze_candidates(isa);
        let encode = |m: &str, ops: &[String]| -> Option<Vec<u8>> {
            match isa {
                "aarch64" => encode_aarch64(m, ops),
                "riscv64" => encode_riscv64(m, ops),
                "arm" => encode_arm(m, ops),
                "mips" => encode_mips(m, ops, is_64bit),
                _ => None,
            }
        };
        let mut rows: Vec<String> = Vec::new();
        for m in mnemonics {
            for arity in 0..=4usize {
                // arity 越高池子越小, 避免组合爆炸同时覆盖主要形态
                let n = match arity {
                    0 | 1 | 2 => pool.len(),
                    3 => pool.len().min(20),
                    _ => pool.len().min(6),
                };
                // odometer 枚举 arity 个操作数的全部组合
                let mut idx: Vec<Vec<usize>> = Vec::new();
                if arity == 0 {
                    idx.push(Vec::new());
                } else {
                    let mut combo = vec![0usize; arity];
                    loop {
                        idx.push(combo.clone());
                        let mut car = true;
                        for slot in combo.iter_mut().rev() {
                            let v = *slot + 1;
                            if v >= n {
                                *slot = 0;
                            } else {
                                *slot = v;
                                car = false;
                                break;
                            }
                        }
                        if car {
                            break;
                        }
                    }
                }
                for pick in idx {
                    let ops: Vec<String> = pick.iter().map(|&i| pool[i].to_string()).collect();
                    if let Some(bytes) = encode(m, &ops) {
                        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                        rows.push(format!("{isa}|{m}|{} => {hex}", ops.join("|")));
                    }
                }
            }
        }
        rows.sort();
        rows.dedup();
        rows
    }

    /// FNV-1a 64bit: 无依赖的确定摘要, 用于把完整语料压缩成可入库的基线。
    fn fnv1a64(s: &str) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in s.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        h
    }

    /// 冻结报告 = 逐 ISA 的行数+摘要 (精确钉住行为) + 等距抽样行 (可诊断)。
    /// 全量语料达百 MB 且高度冗余, 不入库; 摘要失配时跑 regen 看 diff。
    fn freeze_report() -> String {
        let mut out = String::new();
        out.push_str("# mini_asm 编码行为冻结基线 (双向表改造的差分对照)\n");
        out.push_str("# 重新生成: cargo test --lib encode_freeze -- --ignored\n");
        out.push_str("# DIGEST 行钉住「全部可编码输入 -> 字节」的完整行为; SAMPLE 行为等距抽样, 供人工定位。\n");
        // 每个 ISA 只算一次: (标签, mini_asm 架构键, is_64bit)
        let sets = [
            ("aarch64", "aarch64", false),
            ("riscv64", "riscv64", false),
            ("arm", "arm", false),
            ("mips32", "mips", false),
            ("mips64", "mips", true),
        ];
        let mut all: Vec<String> = Vec::new();
        for (label, key, is64) in sets {
            let rows = freeze_rows(key, is64);
            let joined = rows.join("\n");
            out.push_str(&format!(
                "DIGEST {label} rows={} bytes={} fnv1a64={:016x}\n",
                rows.len(),
                joined.len(),
                fnv1a64(&joined)
            ));
            all.extend(rows);
        }
        let stride = (all.len() / 240).max(1);
        out.push_str("# SAMPLE\n");
        for (i, r) in all.iter().enumerate() {
            if i % stride == 0 {
                out.push_str(r);
                out.push('\n');
            }
        }
        out
    }

    /// 钉住改造前后的编码行为完全一致 (行数 + 逐字节摘要)。
    #[test]
    fn encode_freeze_is_byte_identical() {
        let golden = include_str!("../../../tests/fixtures/asm/mini_asm_encode_freeze.txt");
        let got = freeze_report();
        assert_eq!(
            got, golden,
            "mini_asm 编码行为与冻结基线不一致; 若为有意变更, 跑 --ignored regen 并审阅 diff",
        );
    }

    /// 维护入口: 显式重新生成冻结基线。只有在**有意**改变编码行为时才运行,
    /// 随后必须人工审阅 diff。
    #[test]
    #[ignore = "regenerates the golden freeze baseline; run deliberately"]
    fn regenerate_encode_freeze() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/asm/mini_asm_encode_freeze.txt");
        std::fs::write(path, freeze_report()).unwrap();
    }

    #[test]
    fn test_parse_aarch64_regs() {
        assert_eq!(parse_aarch64_reg("x0"), Some((0, true)));
        assert_eq!(parse_aarch64_reg("w15"), Some((15, false)));
        assert_eq!(parse_aarch64_reg("sp"), Some((31, true)));
        assert_eq!(parse_aarch64_reg("lr"), Some((30, true)));
        assert_eq!(parse_aarch64_reg("xzr"), Some((31, true)));
    }

    // ===== 操作数严格语义: 解析不出不得默认成寄存器 0 ==============

    fn ops(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// `blr xN` 曾被静默编成 `blr x0` —— 最难查的一类 silent miscompile。
    #[test]
    fn aarch64_unparseable_mandatory_operand_is_rejected_not_zeroed() {
        // 正面: 真实寄存器必须被编码到对应位域
        let x9 = encode_aarch64("blr", &ops(&["x9"])).unwrap();
        let x0 = encode_aarch64("blr", &ops(&["x0"])).unwrap();
        assert_ne!(x9, x0, "blr 的目标寄存器必须影响编码");
        // 反面: 非法/缺失操作数一律 None, 绝不等于 x0 的编码
        assert_eq!(encode_aarch64("blr", &ops(&["garbage"])), None);
        assert_eq!(encode_aarch64("blr", &ops(&[])), None);
        assert_eq!(encode_aarch64("br", &ops(&["[x1]"])), None);
        assert_eq!(encode_aarch64("svc", &ops(&["#notanum"])), None);
    }

    /// 带括号基址的 SVE/NEON 形式曾被默默当成 x0 + v0。
    #[test]
    fn aarch64_vector_forms_do_not_silently_default_bases_and_lanes() {
        // operands[1] 不是裸寄存器 -> 不能默默用 x0
        assert_eq!(encode_aarch64("ld1w", &ops(&["z0.s", "[x3]"])), None);
        assert_eq!(encode_aarch64("st1w", &ops(&["z0.s", "[x3]"])), None);
        // 逐元素宽度不一致的混合操作数不能当作 v0
        assert_eq!(encode_aarch64("fadd", &ops(&["v0.2s", "x1", "x2"])), None);
        assert_eq!(encode_aarch64("add", &ops(&["v0.16b", "garbage", "v2.16b"])), None);
        assert_eq!(encode_aarch64("whilelt", &ops(&["p0.d", "[x1]", "x2"])), None);
        // 合法形式仍必须成功
        assert!(encode_aarch64("ld1w", &ops(&["z0.s", "x3"])).is_some());
        assert!(encode_aarch64("fadd", &ops(&["v0.2s", "v1.2s", "v2.2s"])).is_some());
    }

    /// 可省略操作数: 缺省合法, 但给了就必须合法。
    #[test]
    fn optional_operands_default_only_when_absent() {
        // sfence.vma 不带操作数 = 刷全部, 合法
        assert!(encode_riscv64("sfence.vma", &ops(&[])).is_some());
        // 带了非法操作数不得默默当成 zero
        assert_eq!(encode_riscv64("sfence.vma", &ops(&["garbage"])), None);
        let a = encode_riscv64("sfence.vma", &ops(&[])).unwrap();
        let b = encode_riscv64("sfence.vma", &ops(&["a0"])).unwrap();
        assert_ne!(a, b, "给出 rs1 必须反映到编码里");
        // mips sync stype
        assert!(encode_mips("sync", &ops(&[]), false).is_some());
        assert_eq!(encode_mips("sync", &ops(&["garbage"]), false), None);
    }

    /// GAS 两种立即数写法 (`#0x..` 与十进制) 都得支持, 之前只支持十进制。
    #[test]
    fn hex_and_decimal_immediates_both_encode() {
        assert_eq!(
            encode_aarch64("movz", &ops(&["x0", "#0x1ff"])),
            encode_aarch64("movz", &ops(&["x0", "#511"]))
        );
        assert!(encode_aarch64("svc", &ops(&["#0x10"])).is_some());
        assert!(encode_arm("svc", &ops(&["#0x123456"])).is_some());
    }

    #[test]
    fn test_encode_aarch64_nop() {
        let bytes = encode_aarch64("nop", &[]).unwrap();
        assert_eq!(bytes, vec![0x1f, 0x20, 0x03, 0xd5]);
    }

    #[test]
    fn test_encode_aarch64_dmb() {
        let bytes = encode_aarch64("dmb", &["ish".to_string()]).unwrap();
        assert_eq!(bytes, vec![0xbf, 0x3b, 0x03, 0xd5]);
    }

    #[test]
    fn test_encode_aarch64_ret() {
        let bytes = encode_aarch64("ret", &[]).unwrap();
        assert_eq!(bytes, vec![0xc0, 0x03, 0x5f, 0xd6]);
    }

    #[test]
    fn test_parse_riscv_regs() {
        assert_eq!(parse_riscv_reg("x0"), Some(0));
        assert_eq!(parse_riscv_reg("zero"), Some(0));
        assert_eq!(parse_riscv_reg("ra"), Some(1));
        assert_eq!(parse_riscv_reg("sp"), Some(2));
        assert_eq!(parse_riscv_reg("a0"), Some(10));
        assert_eq!(parse_riscv_reg("t0"), Some(5));
    }

    #[test]
    fn test_encode_riscv_nop() {
        let bytes = encode_riscv64("nop", &[]).unwrap();
        assert_eq!(bytes, vec![0x13, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_encode_riscv_fence() {
        let bytes = encode_riscv64("fence", &[]).unwrap();
        assert_eq!(bytes, vec![0x0f, 0x00, 0xf0, 0x0f]);
    }

    #[test]
    fn test_encode_arm_nop() {
        let bytes = encode_arm("nop", &[]).unwrap();
        assert_eq!(bytes, vec![0x00, 0x00, 0xa0, 0xe1]);
    }

    #[test]
    fn test_encode_arm_dmb() {
        let bytes = encode_arm("dmb", &["ish".to_string()]).unwrap();
        assert_eq!(bytes, vec![0x5b, 0xf0, 0x7f, 0xf5]);
    }

    #[test]
    fn test_parse_mips_regs() {
        assert_eq!(parse_mips_reg("$zero"), Some(0));
        assert_eq!(parse_mips_reg("zero"), Some(0));
        assert_eq!(parse_mips_reg("$ra"), Some(31));
        assert_eq!(parse_mips_reg("$sp"), Some(29));
        assert_eq!(parse_mips_reg("$0"), Some(0));
        assert_eq!(parse_mips_reg("$31"), Some(31));
    }

    #[test]
    fn test_encode_mips_nop() {
        let bytes = encode_mips("nop", &[], false).unwrap();
        assert_eq!(bytes, vec![0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_encode_mips_sync() {
        let bytes = encode_mips("sync", &[], false).unwrap();
        assert_eq!(bytes, vec![0x00, 0x00, 0x00, 0x0f]);
    }

    // ===== NEON SIMD tests =====

    #[test]
    fn test_neon_reg_parse() {
        // v prefix = 128-bit
        assert_eq!(parse_neon_reg("v0"), Some((0, 128)));
        assert_eq!(parse_neon_reg("v31"), Some((31, 128)));
        // d prefix = 64-bit
        assert_eq!(parse_neon_reg("d0"), Some((0, 64)));
        // s prefix = 32-bit
        assert_eq!(parse_neon_reg("s15"), Some((15, 32)));
        // with arrangement specifier (should be stripped)
        assert_eq!(parse_neon_reg("v0.16b"), Some((0, 128)));
        assert_eq!(parse_neon_reg("v0.4s"), Some((0, 128)));
        assert_eq!(parse_neon_reg("v0.2d"), Some((0, 128)));
        // out of range
        assert_eq!(parse_neon_reg("v32"), None);
    }

    #[test]
    fn test_is_neon_reg() {
        assert!(is_neon_reg("v0"));
        assert!(is_neon_reg("d15"));
        assert!(is_neon_reg("s31"));
        assert!(is_neon_reg("h0"));
        assert!(is_neon_reg("b7"));
        assert!(!is_neon_reg("x0"));
        assert!(!is_neon_reg("w1"));
        assert!(!is_neon_reg("z0"));
    }

    #[test]
    fn test_encode_neon_fadd_vector() {
        // FADD v0.2d, v1.2d, v2.2d
        let ops = vec!["v0".to_string(), "v1".to_string(), "v2".to_string()];
        let bytes = encode_aarch64("fadd", &ops).unwrap();
        assert_eq!(bytes.len(), 4);
        // Q=1 (128-bit), size=10 (.2d)
        let insn = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(insn, 0x4ea2d420);
    }

    #[test]
    fn test_encode_neon_add_vector() {
        // ADD v0.16b, v1.16b, v2.16b
        let ops = vec!["v0".to_string(), "v1".to_string(), "v2".to_string()];
        let bytes = encode_aarch64("add", &ops).unwrap();
        assert_eq!(bytes.len(), 4);
        let insn = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(insn, 0x4e228420);
    }

    #[test]
    fn test_encode_neon_ld1() {
        // LD1 v0, [x1]
        let ops = vec!["v0".to_string(), "x1".to_string()];
        let bytes = encode_aarch64("ld1", &ops).unwrap();
        assert_eq!(bytes.len(), 4);
        let insn = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(insn, 0x4c407020);
    }

    #[test]
    fn test_encode_neon_st1() {
        // ST1 v3, [x5]
        let ops = vec!["v3".to_string(), "x5".to_string()];
        let bytes = encode_aarch64("st1", &ops).unwrap();
        assert_eq!(bytes.len(), 4);
        let insn = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(insn, 0x4c0070a3);
    }

    // ===== SVE tests =====

    #[test]
    fn test_sve_reg_parse() {
        assert_eq!(parse_sve_reg("z0"), Some(0));
        assert_eq!(parse_sve_reg("z15"), Some(15));
        assert_eq!(parse_sve_reg("z31"), Some(31));
        assert_eq!(parse_sve_reg("z32"), None);
    }

    #[test]
    fn test_sve_pred_reg_parse() {
        assert_eq!(parse_sve_pred_reg("p0"), Some(0));
        assert_eq!(parse_sve_pred_reg("p15"), Some(15));
        assert_eq!(parse_sve_pred_reg("p16"), None);
    }

    #[test]
    fn test_encode_sve_ptrue() {
        // PTRUE p0
        let ops = vec!["p0".to_string()];
        let bytes = encode_aarch64("ptrue", &ops).unwrap();
        assert_eq!(bytes.len(), 4);
        let insn = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(insn, 0x25b86000);
    }

    #[test]
    fn test_encode_sve_whilelt() {
        // WHILELT p0, x0, x1
        let ops = vec!["p0".to_string(), "x0".to_string(), "x1".to_string()];
        let bytes = encode_aarch64("whilelt", &ops).unwrap();
        assert_eq!(bytes.len(), 4);
        let insn = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(insn, 0x2528a420);
    }

    #[test]
    fn test_encode_sve_add() {
        // ADD z0, z1, z2
        let ops = vec!["z0".to_string(), "z1".to_string(), "z2".to_string()];
        let bytes = encode_aarch64("add", &ops).unwrap();
        assert_eq!(bytes.len(), 4);
        let insn = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(insn, 0x45020420);
    }

    #[test]
    fn test_encode_sve_ld1w() {
        // LD1W z0, [x1]
        let ops = vec!["z0".to_string(), "x1".to_string()];
        let bytes = encode_aarch64("ld1w", &ops).unwrap();
        assert_eq!(bytes.len(), 4);
        let insn = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(insn, 0x85800020);
    }
}
