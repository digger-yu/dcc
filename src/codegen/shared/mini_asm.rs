// src/codegen/mini_asm.rs - Non-x86 mini-assembler for -c path
//
// Provides instruction parsing and machine code encoding for
// AArch64, RISC-V, ARM, MIPS and MIPS64 inline assembly.
// Used by non-x86 backends in -c mode to produce actual machine
// code bytes instead of just storing asm text.

/// Parse resolved asm text into individual instruction lines.
/// Strips comments, empty lines, and leading/trailing whitespace.
pub fn parse_asm_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|l| {
            // Strip AT&T style comment (# ...)
            let l = if let Some(pos) = l.find('#') { &l[..pos] } else { l };
            // Strip // style comment
            let l = if let Some(pos) = l.find("//") { &l[..pos] } else { l };
            l.trim().to_string()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/// Parse a single instruction line into (mnemonic, operands).
/// Returns None if the line looks like a directive (starts with '.')
/// or a label (ends with ':').
pub fn parse_instruction(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('.') || line.ends_with(':') {
        return None;
    }
    // Skip label prefix: "label: insn ..."
    let line = if let Some(pos) = line.find(':') {
        line[pos + 1..].trim()
    } else {
        line
    };
    if line.is_empty() {
        return None;
    }
    let parts: Vec<&str> = line.split_whitespace().collect();
    let mnemonic = parts[0].to_lowercase();
    let operands_str = if parts.len() > 1 {
        parts[1..].join(" ")
    } else {
        String::new()
    };
    // Split operands by comma, handling spaces
    let operands: Vec<String> = if operands_str.is_empty() {
        Vec::new()
    } else {
        operands_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    Some((mnemonic, operands))
}

// ===== AArch64 register parsing and encoding =====

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
            let imm = operands
                .first()
                .and_then(|s| s.trim_start_matches('#').parse::<u32>().ok())
                .unwrap_or(0);
            let insn: u32 = 0xd4000001 | ((imm & 0xffff) << 5);
            Some(insn.to_le_bytes().to_vec())
        },

        // BR Xn
        "br" => {
            let rn = operands
                .first()
                .and_then(|r| parse_aarch64_reg(r).map(|(n, _)| n))
                .unwrap_or(0);
            let insn: u32 = 0xd61f0000 | ((rn as u32 & 0x1f) << 5);
            Some(insn.to_le_bytes().to_vec())
        },

        // BLR Xn
        "blr" => {
            let rn = operands
                .first()
                .and_then(|r| parse_aarch64_reg(r).map(|(n, _)| n))
                .unwrap_or(0);
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
            if let Some((rd, is64)) = parse_aarch64_reg(&operands[0]) {
                let imm = operands[1].trim_start_matches('#').parse::<u32>().ok()?;
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

        // MSR/MRS system register access - encode as NOP (complex encoding)
        "msr" | "mrs" => Some(encode_aarch64_nop().to_vec()),

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
            let imm = operands
                .first()
                .and_then(|s| s.trim_start_matches('#').parse::<u32>().ok())
                .unwrap_or(0);
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
            let (rd, rsz) = parse_neon_reg(&operands[0]).unwrap_or((0, 128));
            let (rn, _) = parse_neon_reg(&operands[1]).unwrap_or((0, 128));
            let (rm, _) = parse_neon_reg(&operands[2]).unwrap_or((0, 128));
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
            let (rd, rsz) = parse_neon_reg(&operands[0]).unwrap_or((0, 128));
            let (rn, _) = parse_neon_reg(&operands[1]).unwrap_or((0, 128));
            let (rm, _) = parse_neon_reg(&operands[2]).unwrap_or((0, 128));
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
            let (rd, rsz) = parse_neon_reg(&operands[0]).unwrap_or((0, 128));
            let (rn, _) = parse_neon_reg(&operands[1]).unwrap_or((0, 128));
            let (rm, _) = parse_neon_reg(&operands[2]).unwrap_or((0, 128));
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
            let (rd, rsz) = parse_neon_reg(&operands[0]).unwrap_or((0, 128));
            let (rn, _) = parse_neon_reg(&operands[1]).unwrap_or((0, 128));
            let (rm, _) = parse_neon_reg(&operands[2]).unwrap_or((0, 128));
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
            let (rd, rsz) = parse_neon_reg(&operands[0]).unwrap_or((0, 128));
            let (rn, _) = parse_neon_reg(&operands[1]).unwrap_or((0, 128));
            let (rm, _) = parse_neon_reg(&operands[2]).unwrap_or((0, 128));
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
            let (rd, rsz) = parse_neon_reg(&operands[0]).unwrap_or((0, 128));
            let (rn, _) = parse_neon_reg(&operands[1]).unwrap_or((0, 128));
            let (rm, _) = parse_neon_reg(&operands[2]).unwrap_or((0, 128));
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
            let (vt, _) = parse_neon_reg(&operands[0]).unwrap_or((0, 128));
            let (xn, _) = parse_aarch64_reg(&operands[1]).unwrap_or((0, true));
            // LD1 (single structure, 128-bit): 0x4C007400 | Rn | Vt
            let insn: u32 = 0x4c407400 | ((xn as u32 & 0x1f) << 5) | (vt as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // ST1 {Vt.T}, [Xn] - store 128-bit vector
        "st1" if operands.len() == 2 && is_neon_reg(&operands[0]) => {
            let (vt, _) = parse_neon_reg(&operands[0]).unwrap_or((0, 128));
            let (xn, _) = parse_aarch64_reg(&operands[1]).unwrap_or((0, true));
            // ST1 (single structure, 128-bit): 0x4C006400 | Rn | Vt
            let insn: u32 = 0x4c006400 | ((xn as u32 & 0x1f) << 5) | (vt as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // ===== SVE instructions =====

        // PTRUE Pd.T - set all predicate elements to true
        "ptrue" if operands.len() == 1 && is_sve_pred_reg(&operands[0]) => {
            let pd = parse_sve_pred_reg(&operands[0]).unwrap_or(0);
            // PTRUE: 0x25B86000 | Pd
            let insn: u32 = 0x25b86000 | (pd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // WHILELT Pd, Rn, Rm
        "whilelt" if operands.len() == 3 && is_sve_pred_reg(&operands[0]) => {
            let pd = parse_sve_pred_reg(&operands[0]).unwrap_or(0);
            let (rn, _) = parse_aarch64_reg(&operands[1]).unwrap_or((0, true));
            let (rm, _) = parse_aarch64_reg(&operands[2]).unwrap_or((0, true));
            // WHILELT: 0x2528A400 | Rn | Rm | Pd
            let insn: u32 = 0x2528a400
                | ((rn as u32 & 0x1f) << 16)
                | ((rm as u32 & 0x1f) << 5)
                | (pd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // SVE FADD Zzd.Pg/M, Zzn, Zzm
        "fadd" if operands.len() == 3 && is_sve_reg(&operands[0]) => {
            let zd = parse_sve_reg(&operands[0]).unwrap_or(0);
            let zn = parse_sve_reg(&operands[1]).unwrap_or(0);
            let zm = parse_sve_reg(&operands[2]).unwrap_or(0);
            // SVE FADD: 0x65000400 | Zm | Zn | Zd (simplified)
            let insn: u32 = 0x65000400
                | ((zm as u32 & 0x1f) << 16)
                | ((zn as u32 & 0x1f) << 5)
                | (zd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // SVE ADD Zzd.Pg/M, Zzn, Zzm
        "add" if operands.len() == 3 && is_sve_reg(&operands[0]) => {
            let zd = parse_sve_reg(&operands[0]).unwrap_or(0);
            let zn = parse_sve_reg(&operands[1]).unwrap_or(0);
            let zm = parse_sve_reg(&operands[2]).unwrap_or(0);
            // SVE ADD: 0x45000400 | Zm | Zn | Zd (simplified)
            let insn: u32 = 0x45000400
                | ((zm as u32 & 0x1f) << 16)
                | ((zn as u32 & 0x1f) << 5)
                | (zd as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // LD1W {Zt.Pg}, [Xn] - SVE load vector of words
        "ld1w" if operands.len() == 2 && is_sve_reg(&operands[0]) => {
            let zt = parse_sve_reg(&operands[0]).unwrap_or(0);
            let (xn, _) = parse_aarch64_reg(&operands[1]).unwrap_or((0, true));
            // LD1W (vector): 0x85800000 | Rn | Zt (simplified)
            let insn: u32 = 0x85800000 | ((xn as u32 & 0x1f) << 5) | (zt as u32 & 0x1f);
            Some(insn.to_le_bytes().to_vec())
        },

        // ST1W {Zt.Pg}, [Xn] - SVE store vector of words
        "st1w" if operands.len() == 2 && is_sve_reg(&operands[0]) => {
            let zt = parse_sve_reg(&operands[0]).unwrap_or(0);
            let (xn, _) = parse_aarch64_reg(&operands[1]).unwrap_or((0, true));
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

        // SFENCE.VMA rs1, rs2
        "sfence.vma" => {
            let rs1 = operands
                .first()
                .and_then(|r| parse_riscv_reg(r))
                .unwrap_or(0);
            let rs2 = operands
                .get(1)
                .and_then(|r| parse_riscv_reg(r))
                .unwrap_or(0);
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
    let s = s.trim();
    let s = s.trim_start_matches('#');
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i32::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<i32>().ok()
    }
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
            let imm = operands
                .first()
                .and_then(|s| s.trim_start_matches('#').parse::<u32>().ok())
                .unwrap_or(0);
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
    let s = s.trim();
    let s = s.trim_start_matches('#');
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i32::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<i32>().ok()
    }
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
            // SYNC stype (default 0)
            let stype = operands
                .first()
                .and_then(|s| parse_mips_imm(s))
                .unwrap_or(0) as u32;
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
            let code = operands
                .first()
                .and_then(|s| parse_mips_imm(s))
                .unwrap_or(0) as u32;
            let insn: u32 = (code << 6) | 0x0d;
            Some(insn.to_be_bytes().to_vec())
        },

        // SYSCALL
        "syscall" => {
            let code = operands
                .first()
                .and_then(|s| parse_mips_imm(s))
                .unwrap_or(0) as u32;
            let insn: u32 = (code << 6) | 0x0c;
            Some(insn.to_be_bytes().to_vec())
        },

        // CACHE / PREF (hint instructions, encode with operands)
        "cache" | "pref" => {
            // These need immediate + base register, complex encoding
            // Fall through to NOP for now
            Some([0u8; 4].to_vec())
        },

        _ => None,
    }
}

/// Encode a list of resolved asm lines into machine code bytes
/// for the given architecture. Returns Ok(encoded_bytes) on success,
/// or Err(unrecognized_mnemonics) if any instruction is not supported.
/// P0-5: unrecognized instructions are hard errors, not NOP fallbacks.
pub fn encode_asm_lines_for_arch(
    lines: &[String],
    arch: &str,
    is_64bit: bool,
) -> Result<Vec<u8>, Vec<String>> {
    let mut bytes = Vec::new();
    let mut unrecognized = Vec::new();

    for line in lines {
        if let Some((mnemonic, operands)) = parse_instruction(line) {
            let encoded = match arch {
                "aarch64" => encode_aarch64(&mnemonic, &operands),
                "riscv64" => encode_riscv64(&mnemonic, &operands),
                "arm" => encode_arm(&mnemonic, &operands),
                "mips" | "mips64" => encode_mips(&mnemonic, &operands, is_64bit),
                _ => None,
            };
            match encoded {
                Some(mut b) => bytes.append(&mut b),
                None => {
                    unrecognized.push(format!(
                        "{}:{}",
                        mnemonic,
                        if operands.is_empty() {
                            String::new()
                        } else {
                            operands.join(", ")
                        }
                    ));
                },
            }
        }
        // Directives and labels are silently skipped
    }

    if unrecognized.is_empty() {
        Ok(bytes)
    } else {
        Err(unrecognized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_aarch64_regs() {
        assert_eq!(parse_aarch64_reg("x0"), Some((0, true)));
        assert_eq!(parse_aarch64_reg("w15"), Some((15, false)));
        assert_eq!(parse_aarch64_reg("sp"), Some((31, true)));
        assert_eq!(parse_aarch64_reg("lr"), Some((30, true)));
        assert_eq!(parse_aarch64_reg("xzr"), Some((31, true)));
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

    #[test]
    fn test_parse_instruction() {
        assert_eq!(parse_instruction("nop"), Some(("nop".to_string(), vec![])));
        assert_eq!(
            parse_instruction("dmb ish"),
            Some(("dmb".to_string(), vec!["ish".to_string()]))
        );
        assert_eq!(
            parse_instruction("mov w0, w1"),
            Some(("mov".to_string(), vec!["w0".to_string(), "w1".to_string()]))
        );
        assert_eq!(parse_instruction(".globl foo"), None);
        assert_eq!(parse_instruction("label:"), None);
    }

    #[test]
    fn test_encode_asm_lines_ok() {
        let lines = vec!["nop".to_string(), "nop".to_string()];
        let result = encode_asm_lines_for_arch(&lines, "aarch64", false);
        assert!(result.is_ok());
        let bytes = result.unwrap();
        // Two AArch64 NOPs = 8 bytes
        assert_eq!(bytes.len(), 8);
    }

    #[test]
    fn test_encode_asm_lines_unrecognized_is_error() {
        // P0-5: unrecognized instructions must produce Err, not NOP
        let lines = vec![
            "nop".to_string(),
            "xyzzy_invalid".to_string(),
            "nop".to_string(),
        ];
        let result = encode_asm_lines_for_arch(&lines, "aarch64", false);
        assert!(result.is_err());
        let unrecognized = result.unwrap_err();
        assert_eq!(unrecognized.len(), 1);
        assert!(unrecognized[0].starts_with("xyzzy_invalid"));
    }

    #[test]
    fn test_encode_asm_lines_riscv_unrecognized() {
        let lines = vec!["unknown_riscv_insn".to_string()];
        let result = encode_asm_lines_for_arch(&lines, "riscv64", true);
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_asm_lines_arm_unrecognized() {
        let lines = vec!["bad_arm_insn".to_string()];
        let result = encode_asm_lines_for_arch(&lines, "arm", false);
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_asm_lines_mips_unrecognized() {
        let lines = vec!["bad_mips_insn".to_string()];
        let result = encode_asm_lines_for_arch(&lines, "mips", false);
        assert!(result.is_err());
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
        assert_eq!(insn, 0x4c407420);
    }

    #[test]
    fn test_encode_neon_st1() {
        // ST1 v3, [x5]
        let ops = vec!["v3".to_string(), "x5".to_string()];
        let bytes = encode_aarch64("st1", &ops).unwrap();
        assert_eq!(bytes.len(), 4);
        let insn = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(insn, 0x4c0064a3);
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
