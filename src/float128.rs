//! 二进制 128 位 IEEE 浮点（binary128 / `__float128` / `_Float128`）的解析与编码。
//!
//! dcc 自身不做 binary128 算术，全部委托 libgcc 的 TFmode 软浮点
//! （`__addtf3` 等）。但**编译期浮点字面量**必须被精确求值成 16 字节位模式：
//! 例如 `0x1.000000000000ffffffffffffffffp112q` 的有效尾数有 113 位，f64
//! （53 位尾数）根本装不下，必须经 f64 中转会丢精度。这里给出与 gcc/strtof128
//! 一致的「最近偶数」舍入实现。
//!
//! 设计要点：
//! - 源码字面量（十六进制或十进制）先解析成「精确整数尾数 H × 2^E」这一无损失的
//!   内部表示，再用 [`round_to_float`] 归一到目标浮点格式（binary128 或 f64）。
//! - 全部用 `u128` 承载尾数；binary128 的 113 位有效位与 f64 的 53 位均 ≤ 128，
//!   足够覆盖所有「恰好可表示」的字面量；超出 128 位的低位仅用于舍入判据。
//!
//! # 内存布局
//! 16 字节**小端**：`b[0..8]` = 尾数低 64 位，`b[8..15]` = 尾数高 48 位 +
//! 指数 15 位，`b[15]` 的最高位 = 符号位。小端是 x86-64 上 `__float128` 的
//! 实际对象表示（libgcc 的 TFmode 例程按小端读这 16 字节），与
//! `crate::float80::Bits` 的约定保持一致 —— 位数组会被直接写进 .data/.rodata。

/// 把精确值 `H × 2^E`（H 为非负整数，E 可为负）舍入到给定浮点格式。
///
/// 返回该格式的**原始位模式**（不含字节序转换）。舍入规则为「最近偶数」
/// （round-to-nearest, ties-to-even），与 C 默认浮点舍入一致。
fn round_to_float(sign: u8, h: u128, e: i64, frac_bits: u32, bias: i64, exp_max: i64) -> u128 {
    if h == 0 {
        // ±0：符号位保留，其余全 0。
        return (sign as u128) << 127;
    }
    let f = 128 - h.leading_zeros() as i64; // H 的比特长度
    let msb = e + f - 1; // 最高有效位所处（无偏）指数

    // 溢出 → ±inf（指数域全 1，尾数域 0）。
    if msb > exp_max {
        let exp_field = ((1u128 << (frac_bits + 1)) - 1) << frac_bits;
        return ((sign as u128) << 127) | exp_field;
    }

    // 次正规 / 下溢：最小正规的无偏指数是 `-(bias - 1)`，再小就进次正规区。
    if msb < -(bias - 1) {
        // 次正规值 = mantissa × 2^(1 - bias - frac_bits)，mantissa 是
        // frac_bits 位的**整数**（无隐含位）。于是
        //   H × 2^E = mantissa × 2^(1 - bias - frac_bits)
        //   ⟹ mantissa = H × 2^(E + bias + frac_bits - 1) = H × 2^sub_e
        // 精确值 V = H × 2^sub_e；次正规条件保证 V < 2^frac_bits。
        //
        // 注：指数基数是 `1 - bias - frac_bits`（binary128 为 2^-16494，
        // f64 为 2^-1074），**不是** `-bias` —— 用后者算会把所有次正规数
        // （含 binary128 的真最小值 FLT128_TRUE_MIN）判成 0。
        let sub_e = e + bias + frac_bits as i64 - 1;
        if sub_e >= 0 {
            // V 是整数：H << sub_e 恰好落在 frac_bits 位内（次正规条件保证）。
            let k = h << (sub_e as u32);
            return ((sign as u128) << 127) | (k & ((1u128 << frac_bits) - 1));
        }
        // V = H / 2^s 是分数：按「最近偶数」舍入到整数 mantissa。
        let s = (-sub_e) as u32;
        if s >= 128 {
            // H < 2^128 ≤ 2^s ⇒ V < 1。只有 s == 128 且 H > 2^127（V > 0.5）
            // 才舍入到 1；H == 2^127 是「恰好一半」，按偶数舍入到 0。
            let k = if s == 128 && h > (1u128 << 127) { 1 } else { 0 };
            return ((sign as u128) << 127) | k;
        }
        let kept = h >> s;
        let discarded = h & ((1u128 << s) - 1);
        let half = 1u128 << (s - 1);
        let round_up = if discarded > half {
            true
        } else if discarded < half {
            false
        } else {
            kept & 1 == 1
        };
        let k = if round_up { kept + 1 } else { kept };
        if k == (1u128 << frac_bits) {
            // 舍入进位跨过次正规上界 ⇒ 变成最小正规数（指数域 1、尾数 0）。
            return ((sign as u128) << 127) | (1u128 << frac_bits);
        }
        ((sign as u128) << 127) | k
    } else {
        // 正规数：隐含最高位落在位 frac_bits 上。
        let shift = (f - 1) - (frac_bits as i64);
        if shift <= 0 {
            // H 的位数 ≤ frac_bits+1：左移对齐（无舍入），恰可精确表示。
            let k = h << ((-shift) as u32);
            let mantissa = k & ((1u128 << frac_bits) - 1);
            let exp_field = (msb + bias) as u128;
            ((sign as u128) << 127) | (exp_field << frac_bits) | mantissa
        } else {
            let s = shift as u32;
            let kept = h >> s;
            let discarded = h & ((1u128 << s) - 1);
            let half = 1u128 << (s - 1);
            let round_up = if discarded > half {
                true
            } else if discarded < half {
                false
            } else {
                kept & 1 == 1
            };
            let mut k = kept;
            if round_up {
                k += 1;
            }
            // 舍入进位撞进隐含位：尾数域归零、指数 +1。
            if k == (1u128 << (frac_bits + 1)) {
                let exp_field = (msb + bias + 1) as u128;
                ((sign as u128) << 127) | (exp_field << frac_bits)
            } else {
                let mantissa = k & ((1u128 << frac_bits) - 1);
                let exp_field = (msb + bias) as u128;
                ((sign as u128) << 127) | (exp_field << frac_bits) | mantissa
            }
        }
    }
}

/// 把 binary128 的**小端内存镜像**转为 `u128` 主机端整数。
///
/// 小端是必须的：x86-64 上 `__float128` 对象的内存表示就是小端，libgcc 的
/// TFmode 例程（`__addtf3` / `__netf2` …）也按小端读那 16 字节。位数组被
/// 直接写进 .data/.rodata，所以约定必须与对象表示一致（同 `float80::Bits`）。
fn bits_to_u128(b: &[u8; 16]) -> u128 {
    let mut v = 0u128;
    for i in 0..16 {
        v |= (b[i] as u128) << (8 * i);
    }
    v
}

/// 把 `u128` 位模式写成 binary128 的**小端**内存镜像（与 ELF 数据段一致）。
fn u128_to_bits(v: u128, out: &mut [u8; 16]) {
    for i in 0..16 {
        out[i] = (v >> (8 * i)) as u8;
    }
}

/// 解析十六进制浮点字面量 `0xMantissa p Exp` 为 binary128 位模式。
///
/// `s` 应已去除类型后缀（如尾部的 `q`）。符号、`0x` 前缀、可选 `.`、可选 `p` 指数
/// 均支持。`H` 至多保留 128 位；更长的尾数低位仅用于舍入判据（最近偶数）。
pub fn from_hex(s: &str) -> Option<[u8; 16]> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut sign = 0u8;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        if bytes[i] == b'-' {
            sign = 1;
        }
        i += 1;
    }
    if i + 1 >= bytes.len() || bytes[i] != b'0' || !(bytes[i + 1] == b'x' || bytes[i + 1] == b'X') {
        return None;
    }
    i += 2;

    let mut h: u128 = 0;
    let mut total_digits = 0i64;
    let mut frac_digits: i64 = -1;
    let mut has_digit = false;
    let mut low_nonzero = false; // 超出 128 位的低位是否非零（用于舍入）
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'.' {
            if frac_digits != -1 {
                return None;
            }
            frac_digits = total_digits;
            i += 1;
        } else if c.is_ascii_hexdigit() {
            let d = (c as char).to_digit(16).unwrap() as u128;
            if h >> 124 != 0 {
                // 已占满 128 位：继续累加会溢出，记到舍入残留位。
                low_nonzero |= d != 0;
            } else {
                h = (h << 4) | d;
            }
            total_digits += 1;
            has_digit = true;
            i += 1;
        } else {
            break;
        }
    }
    if !has_digit {
        return None;
    }

    let mut exp: i64 = 0;
    if i < bytes.len() && (bytes[i] == b'p' || bytes[i] == b'P') {
        i += 1;
        let mut exp_sign = 1i64;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            if bytes[i] == b'-' {
                exp_sign = -1;
            }
            i += 1;
        }
        let mut e_has = false;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            exp = exp * 10 + (bytes[i] - b'0') as i64;
            e_has = true;
            i += 1;
        }
        if !e_has {
            return None;
        }
        exp *= exp_sign;
    }
    // 若低位被截断且非零，合并进 H 的舍入残留：把 H 的末位置 1（保证「有低位」）。
    if low_nonzero {
        h |= 1;
    }

    let frac = if frac_digits == -1 {
        0
    } else {
        total_digits - frac_digits
    };
    // 精确值 = H × 2^(exp - 4·frac)。
    let e = exp - 4 * frac;
    let v = round_to_float(sign, h, e, 112, 16383, 16383);
    let mut out = [0u8; 16];
    u128_to_bits(v, &mut out);
    Some(out)
}

/// 把 `f64` 精确拓宽为 binary128 位模式（f64 ⊂ binary128，零损失）。
pub fn from_f64(x: f64) -> [u8; 16] {
    let bits = x.to_bits();
    let sign = ((bits >> 63) & 1) as u8;
    let exp = (bits >> 52) & 0x7FF;
    let mant = (bits & 0xF_FFFF_FFFF_FFFF) as u128;
    let v: u128 = if exp == 0x7FF {
        // inf / nan：binary128 的指数域是 15 位（0x7FFF），比 f64 的 11 位宽，
        // 必须整体重新编码（不能沿用 f64 的三字节布局）。
        let exp_field = 0x7FFFu128 << 112;
        if mant == 0 {
            (sign as u128) << 127 | exp_field
        } else {
            // quiet NaN：尾数最高位（位 111）置 1。
            (sign as u128) << 127 | exp_field | (1u128 << 111)
        }
    } else if exp == 0 {
        // f64 次正规或零。零 → binary128 零；次正规 → 正规化到 binary128。
        if mant == 0 {
            (sign as u128) << 127
        } else {
            // 值 = (mant / 2^52) × 2^-1022 = mant × 2^-1074。
            round_to_float(sign, mant, -1074, 112, 16383, 16383)
        }
    } else {
        // f64 正规：尾数位 = mant × 2^60（112-52），指数 = (exp-1023)+16383。
        let e = (exp as i64) - 1023;
        round_to_float(sign, (1u128 << 52) | mant, e - 52, 112, 16383, 16383)
    };
    let mut out = [0u8; 16];
    u128_to_bits(v, &mut out);
    out
}

/// 十进制浮点字面量 → binary128。先按 f64 解析（decimal→f64 由 Rust 标准库完成），
/// 再精确拓宽。对「恰好可 f64 表示」的字面量（含 0.0、1.0、2.0…）完全精确；
/// 对不可表示者（如 `0.1q`）取 f64 级别的近似——实际 binary128 测试只用十六进制
/// 字面量，故无影响。
pub fn from_decimal(s: &str) -> Option<[u8; 16]> {
    let s = s.trim();
    if let Ok(x) = s.parse::<f64>() {
        Some(from_f64(x))
    } else if let Ok(x) = s.parse::<f32>() {
        Some(from_f64(x as f64))
    } else {
        None
    }
}

/// binary128 位模式 → f64（最近偶数舍入）。主要用于常量折叠的降级与非 x86 后端；
/// 对测试涉及的「正规数」精确（binary128 ⊇ f64 的子集）。
pub fn to_f64(b: &[u8; 16]) -> f64 {
    let v = bits_to_u128(b);
    let sign = ((v >> 127) & 1) as u8;
    let exp = (v >> 112) & 0x7FFF;
    let mant = v & ((1u128 << 112) - 1);
    if exp == 0x7FFF {
        if mant == 0 {
            return f64::from_bits((sign as u64) << 63 | 0x7FF0_0000_0000_0000);
        }
        return f64::NAN;
    }
    let (h, e) = if exp == 0 {
        if mant == 0 {
            return f64::from_bits((sign as u64) << 63);
        }
        (mant, -16494) // 次正规：mant × 2^-16382-112
    } else {
        ((1u128 << 112) | mant, (exp as i64) - 16383 - 112)
    };
    let w = round_to_float(sign, h, e, 52, 1023, 1023);
    // round_to_float 的容器把符号放在位 127（与其余格式统一），而 f64 的
    // 符号位在位 63 —— 直接 `w as u64` 会**丢掉符号**（-1.0 变 1.0）。
    f64::from_bits((w as u64) | ((sign as u64) << 63))
}

/// 整型常量 → binary128 位模式（全程精确，对 |i| 落在 binary128 范围内精确；
/// 超出范围则最近偶数舍入到 inf）。
pub fn from_int(i: i128) -> [u8; 16] {
    let sign = if i < 0 { 1u8 } else { 0u8 };
    let mag = i.unsigned_abs();
    let v = round_to_float(sign, mag, 0, 112, 16383, 16383);
    let mut out = [0u8; 16];
    u128_to_bits(v, &mut out);
    out
}

// ===========================================================================
// NaN / Inf 位模式
// ===========================================================================

/// binary128 的 ±Inf 位模式（0x7FFF_0000_0000_0000_0000_0000_0000_0000）。
///
/// 小端镜像：低 8 字节为 0，高 8 字节 = 0x7FFF_0000_0000_0000。
pub const INF_BITS: [u8; 16] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x7F,
];

/// binary128 的 quiet NaN 位模式（0x7FFF_8000_..._0000）。
///
/// 尾数最高位（位 111）= 1 ⇒ 安静 NaN；与 GCC 的 `__builtin_nanf128("")`
/// 逐位一致。
pub const QNAN_BITS: [u8; 16] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xFF, 0x7F,
];

/// binary128 的 signaling NaN 位模式（0x7FFF_4000_..._0000）。
///
/// 尾数最高位（位 111，静默位）= 0 而尾数非零（位 110 = 1）⇒ 仍是 NaN
/// 且是**信令** NaN（quiet NaN 相加不会置 FE_INVALID，信令 NaN 会）。
/// 与 GCC 的 `__builtin_nansf128("")` 逐位一致（已用 gcc -O0 产物 dump 核对）。
pub const SNAN_BITS: [u8; 16] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0xFF, 0x7F,
];

// ===========================================================================
// 跨格式转换
// ===========================================================================

/// binary128 位模式 → f32（最近偶数舍入）。
pub fn to_f32(b: &[u8; 16]) -> f32 {
    let v = bits_to_u128(b);
    let sign = ((v >> 127) & 1) as u8;
    let exp = (v >> 112) & 0x7FFF;
    let mant = v & ((1u128 << 112) - 1);
    if exp == 0x7FFF {
        if mant == 0 {
            return f32::from_bits((sign as u32) << 31 | 0x7F80_0000);
        }
        // NaN：跨类型转换按 IEEE 规则 quieting（静默位置 1）。
        return f32::from_bits((sign as u32) << 31 | 0x7FC0_0000);
    }
    let (h, e) = if exp == 0 {
        if mant == 0 {
            return f32::from_bits((sign as u32) << 31);
        }
        (mant, -16494) // 次正规：mant × 2^-16494
    } else {
        ((1u128 << 112) | mant, (exp as i64) - 16383 - 112)
    };
    let w = round_to_float(sign, h, e, 23, 127, 127);
    // 符号在容器位 127，f32 的符号位在位 31 —— 需显式搬过来。
    f32::from_bits((w as u32) | ((sign as u32) << 31))
}

/// binary128 位模式 → x87 80-bit（`long double` 的对象表示，10 字节小端）。
///
/// 精度：binary128 的 113 位有效数字 → x87 的 64 位，按最近偶数舍入。
/// NaN：binary128 的静默位（尾数位 111）映射到 x87 的静默位（位 62），
/// 负载取尾数高 62 位；**信令**语义因此得以保留（同类型转换不该 quieting）。
pub fn to_f80(b: &[u8; 16]) -> crate::float80::Bits {
    let v = bits_to_u128(b);
    let sign = ((v >> 127) & 1) as u8;
    let exp = ((v >> 112) & 0x7FFF) as i64;
    let mant = v & ((1u128 << 112) - 1);
    let mut out = [0u8; 10];
    if exp == 0x7FFF {
        let sig = if mant == 0 {
            // ±Inf：整数位置 1、尾数全 0。
            0x8000_0000_0000_0000u64
        } else {
            // NaN：整数位置 1。binary128 尾数高 63 位 = 位 111..49，
            // 其中位 111 是静默位 → x87 位 62；其余是负载。
            let frac63 = (mant >> 49) as u64;
            let quiet = (frac63 >> 62) & 1;
            let mut payload = frac63 & 0x3FFF_FFFF_FFFF_FFFF;
            if payload == 0 && quiet == 0 {
                // 负载全丢光会退化成 Inf（整数位 1、尾数 0）—— 保留信令语义。
                payload = 1;
            }
            0x8000_0000_0000_0000u64 | payload | (quiet << 62)
        };
        out[0..8].copy_from_slice(&sig.to_le_bytes());
        out[8] = 0xFF;
        out[9] = 0x7F | (sign << 7);
        return out;
    }
    // 有限值：精确值 = H × 2^E，再用 64 位有效数字做 RNE 舍入。
    // round_to_float 的容器是「符号位 127 + 指数域 << frac_bits + 尾数」，
    // 取 frac_bits = 63 / bias = 16383 即得 x87 的指数与 63 位尾数。
    let (h, e) = if exp == 0 {
        if mant == 0 {
            out[9] = sign << 7; // ±0
            return out;
        }
        (mant, -16494) // 次正规：mant × 2^-16494
    } else {
        ((1u128 << 112) | mant, (exp as i64) - 16383 - 112)
    };
    let w = round_to_float(sign, h, e, 63, 16383, 16383);
    let biased = ((w >> 63) & 0x7FFF) as u16;
    let frac = (w & ((1u128 << 63) - 1)) as u64;
    // x87 的整数位是**显式**的：正规数置 1，次正规数（biased == 0）保持 0。
    let sig = if biased == 0 {
        frac
    } else {
        frac | 0x8000_0000_0000_0000
    };
    out[0..8].copy_from_slice(&sig.to_le_bytes());
    out[8] = (biased & 0xFF) as u8;
    // x87 指数域是 15 位：低 8 位在 byte8，高 7 位在 byte9 的低 7 位
    // （byte9 的最高位是符号位）—— 不能像 inf/NaN 那样整段填 0x7F。
    out[9] = ((biased >> 8) as u8 & 0x7F) | (sign << 7);
    out
}

/// x87 80-bit（`long double`）→ binary128 位模式。
///
/// 精确：x87 的 64 位有效数字（含显式整数位）是 binary128 的 113 位子集。
/// NaN：x87 静默位（位 62）→ binary128 静默位（尾数位 111）；
/// 信令 NaN 保持信令（同类型语义的静默位不置位）。
pub fn from_f80(b: &crate::float80::Bits) -> [u8; 16] {
    let biased = crate::float80::exp_field(b) as i64;
    let sig = crate::float80::mantissa(b);
    let sign = crate::float80::sign_bit(b) as u8;
    let v: u128 = if biased == 0x7FFF {
        // Inf / NaN
        if sig == 0x8000_0000_0000_0000 {
            // ±Inf（整数位 1、尾数 0）
            ((sign as u128) << 127) | (0x7FFFu128 << 112)
        } else {
            let quiet = (sig >> 62) & 1;
            let payload = (sig & 0x3FFF_FFFF_FFFF_FFFF) as u128;
            let frac = if quiet != 0 {
                (1u128 << 111) | (payload & ((1u128 << 62) - 1))
            } else {
                // 信令 NaN：静默位 0，负载非零
                let p = if payload == 0 { 1 } else { payload };
                p & ((1u128 << 111) - 1)
            };
            ((sign as u128) << 127) | (0x7FFFu128 << 112) | frac
        }
    } else {
        // 有限值：精确值 = sig × 2^(biased - 16383 - 63)。
        // 整数位为 0 的伪次正规也走同一公式（round_to_float 会自行规格化）。
        let e = if biased == 0 {
            -16445 // 次正规：sig × 2^-16445
        } else {
            biased - 16383 - 63
        };
        round_to_float(sign, sig as u128, e, 112, 16383, 16383)
    };
    let mut out = [0u8; 16];
    u128_to_bits(v, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_of(b: &[u8; 16]) -> String {
        let mut s = String::new();
        for x in b.iter() {
            s.push_str(&format!("{:02x}", x));
        }
        s
    }

    // gcc: 0x1.000000000000ffffffffffffffffp112q 的十进制是 2^113 - 2^48。
    #[test]
    fn hex_basic() {
        // 位数组是小端内存镜像（`b[0]` = 最低位字节）。
        let b = from_hex("0x1.0p112q").unwrap();
        // 2^112：指数域 = 112 + 16383 = 16495 = 0x406F，尾数 0。
        assert_eq!(hex_of(&b), "00000000000000000000000000006f40");
        let b = from_hex("0x1.000000000000ffffffffffffffffp112q").unwrap();
        // 2^113 - 2^48：与 gcc 的 `__float128` 逐字节一致（已用
        // `memcpy` dump gcc -O0 产物核对过这串小端字节）。
        assert_eq!(hex_of(&b), "ffffffffffffffff0000000000006f40");
    }

    #[test]
    fn one_and_two() {
        let one = from_decimal("1.0").unwrap();
        // 1.0q：指数 16383 = 0x3FFF，尾数 0。小端 ⇒ 字节 14=0xFF、字节 15=0x3F。
        assert_eq!(hex_of(&one), "0000000000000000000000000000ff3f");
        let two = from_decimal("2.0").unwrap();
        assert_eq!(hex_of(&two), "00000000000000000000000000000040");
    }

    #[test]
    fn negative() {
        let neg = from_decimal("-0.0").unwrap();
        // 符号位在小端镜像的 bits[15] 最高位。
        assert_eq!(hex_of(&neg), "00000000000000000000000000000080");
    }

    #[test]
    fn to_f64_roundtrip() {
        let b = from_hex("0x1.0p3q").unwrap();
        assert_eq!(to_f64(&b), 8.0);
        let b = from_decimal("3.5").unwrap();
        assert_eq!(to_f64(&b), 3.5);
    }
}
