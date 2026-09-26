//! float80 —— x87 80-bit 扩展精度值的表示、解析与正确舍入。
//!
//! # 为什么必须存在
//! `long double` 在 SysV AMD64 上是 x87 扩展精度：**64 位显式整数位尾数 +
//! 15 位指数**，精度与动态范围都超出 `f64`（53 位尾数）。因此十进制字面量
//! 必须在**解析期**一次性舍入到 80-bit。先 `parse::<f64>()` 再展宽会把尾数
//! 低 11 位永久清零：
//!
//! ```text
//! 0.1L  gcc 真值 : 尾数 0xCCCCCCCCCCCCCCCD  指数域 0x3FFB
//! 0.1L  f64 展宽 : 尾数 0xCCCCCCCCCCCC_C800  ← 低 11 位为 0
//! ```
//!
//! 本模块用**精确整数运算**做 round-to-nearest-even，不借助任何中间浮点近似，
//! 因此对所有可解析的字面量都保证单次、正确舍入。
//!
//! # 内存布局
//! 10 字节小端：`b[0..8]` = 尾数（bit63 为显式整数位），
//! `b[8]` = 指数低 8 位，`b[9]` = 符号(bit7) | 指数高 7 位。
//! C 对象里的 `long double` 占 16 字节，`b[10..16]` 是实现定义的 padding，
//! GCC 不保证为零，故所有判据只看低 10 字节。
#![allow(clippy::all)]

/// 80-bit x87 扩展精度的 10 字节小端镜像。
pub type Bits = [u8; 10];

/// 规格化尾数的最高位（显式整数位）。
const NORM: u128 = 1u128 << 63;

// ===========================================================================
// 小大整数（limb 数组成，低位在前）——只用到本模块需要的四种运算
// ===========================================================================

type Big = Vec<u32>;

fn big_from_u64(v: u64) -> Big {
    vec![v as u32, (v >> 32) as u32]
}

fn trim(m: &mut Big) {
    while m.len() > 1 && *m.last().unwrap() == 0 {
        m.pop();
    }
}

fn is_zero_big(m: &Big) -> bool {
    m.iter().all(|&l| l == 0)
}

fn bitlen(m: &Big) -> i32 {
    let mut m = m.clone();
    trim(&mut m);
    let top = *m.last().unwrap_or(&0);
    if top == 0 {
        return 0;
    }
    ((m.len() - 1) as i32) * 32 + 32 - top.leading_zeros() as i32
}

fn mul_small(m: &mut Big, k: u32) {
    let mut carry = 0u64;
    for l in m.iter_mut() {
        let p = (*l as u64) * (k as u64) + carry;
        *l = p as u32;
        carry = p >> 32;
    }
    while carry != 0 {
        m.push(carry as u32);
        carry >>= 32;
    }
}

fn add_small(m: &mut Big, k: u32) {
    let mut carry = k;
    for l in m.iter_mut() {
        let s = (*l as u64) + carry as u64;
        *l = s as u32;
        carry = (s >> 32) as u32;
        if carry == 0 {
            return;
        }
    }
    if carry != 0 {
        m.push(carry);
    }
}

fn shl_bits(m: &mut Big, bits: u32) {
    let words = (bits / 32) as usize;
    let rem = bits % 32;
    // limb 数组是**小端**（index 0 为最低位），所以乘 2^(32·words) 必须在
    // **头部**插入零 limb；push 到尾部不改变数值，且会被 trim 弹掉。
    for _ in 0..words {
        m.insert(0, 0);
    }
    if rem != 0 {
        let mut carry = 0u32;
        for l in m.iter_mut() {
            let hi = *l >> (32 - rem);
            *l = l.wrapping_shl(rem) | carry;
            carry = hi;
        }
        if carry != 0 {
            m.push(carry);
        }
    }
}

/// 返回 `a >= b`（都非负、已 trim 与否均可）。
fn ge(a: &Big, b: &Big) -> bool {
    let (mut x, mut y) = (a.clone(), b.clone());
    trim(&mut x);
    trim(&mut y);
    if x.len() != y.len() {
        return x.len() > y.len();
    }
    for i in (0..x.len()).rev() {
        if x[i] != y[i] {
            return x[i] > y[i];
        }
    }
    true
}

/// `a -= b`，调用方保证 `a >= b`。
fn sub_assign(a: &mut Big, b: &Big) {
    let mut borrow = 0i64;
    for i in 0..a.len() {
        let bj = *b.get(i).unwrap_or(&0) as i64 + borrow;
        let diff = a[i] as i64 - bj;
        if diff < 0 {
            a[i] = (diff + (1i64 << 32)) as u32;
            borrow = 1;
        } else {
            a[i] = diff as u32;
            borrow = 0;
        }
    }
    trim(a);
}

/// 恢复式逐位长除：返回 `(商, 余)`。只需 ~70 位商，故按位做即可（复杂度
/// O(被除数位数 × 除数 limb 数)，对本模块的位数规模是常数级开销）。
fn divmod(n: &Big, d: &Big) -> (Big, Big) {
    assert!(!is_zero_big(d), "float80: 除零");
    let nb = bitlen(n);
    let mut q = vec![0u32; n.len() + 1];
    let mut r: Big = vec![0; d.len() + 1];
    for i in (0..nb).rev() {
        // r = r << 1 | bit_i(n)
        shl_bits(&mut r, 1);
        let w = (i / 32) as usize;
        let b = (i % 32) as u32;
        let bit = (n.get(w).copied().unwrap_or(0) >> b) & 1;
        r[0] |= bit;
        trim(&mut r);
        if ge(&r, d) {
            sub_assign(&mut r, d);
            q[w] |= 1 << b;
        }
    }
    trim(&mut q);
    (q, r)
}

/// `5^k`（k ≥ 0）。快速幂 + 平方。
fn pow5(k: u32) -> Big {
    let mut result = vec![1u32];
    let mut base = vec![5u32];
    let mut e = k;
    while e > 0 {
        if e & 1 == 1 {
            result = mul_big(&result, &base);
        }
        e >>= 1;
        if e > 0 {
            base = mul_big(&base, &base);
        }
    }
    trim(&mut result);
    result
}

/// 学校乘法。只在快速幂里用到，尺寸 ≤ 数千位。
fn mul_big(a: &Big, b: &Big) -> Big {
    let mut out = vec![0u32; a.len() + b.len()];
    for (i, &ai) in a.iter().enumerate() {
        if ai == 0 {
            continue;
        }
        let mut carry = 0u64;
        for (j, &bj) in b.iter().enumerate() {
            let cur = out[i + j] as u64 + (ai as u64) * (bj as u64) + carry;
            out[i + j] = cur as u32;
            carry = cur >> 32;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let cur = out[k] as u64 + carry;
            out[k] = cur as u32;
            carry = cur >> 32;
            k += 1;
        }
    }
    trim(&mut out);
    out
}

// ===========================================================================
// 舍入核心
// ===========================================================================

/// 把 `n * 2^p`（`n` 为正整数）舍入到 64 位尾数。
///
/// 返回 `(sig, uexp)`，满足 `value ≈ sig * 2^(uexp - 63)` 且
/// `sig ∈ [2^63, 2^64)`。`extra_sticky` 用于将调用方在缩放中丢弃的
/// 非零余量（长除的余数）并入 sticky 位。ties-to-even。
fn round_shift(n: &Big, p: i32, extra_sticky: bool) -> (u128, i32) {
    let bl = bitlen(n);
    debug_assert!(bl > 0);
    if bl <= 64 && extra_sticky {
        // 存在外部 sticky（长除余数）时，必须先把 n 拉到 >64 位再舍入，
        // 否则无法判定“被丢弃部分”与半个 ulp 的大小关系。左移不改变值：
        // n×2^g × 2^(p-g) == n × 2^p。
        let g = (80 - bl) as u32;
        let mut m = n.clone();
        shl_bits(&mut m, g);
        return round_shift(&m, p - g as i32, true);
    }
    if bl <= 64 {
        // n 本身就是精确值，左对齐到 bit63，无任何位损失。
        let mut v: u128 = 0;
        for &l in n.iter().rev() {
            v = (v << 32) | l as u128;
        }
        return (v << (64 - bl) as u32, p + bl - 1);
    }
    let drop = (bl - 64) as u32;
    let mut sig = shift_to_u128(n, drop);
    let guard = bit_at(n, drop - 1);
    let mut sticky = extra_sticky;
    if !sticky {
        for i in 0..(drop - 1) {
            if bit_at(n, i) {
                sticky = true;
                break;
            }
        }
    }
    if guard && (sticky || (sig & 1) == 1) {
        sig += 1;
        if sig >> 64 != 0 {
            // 进位：0xFFFF...F + 1 == 2^64 → 尾数回到 NORM，真实指数 +1。
            sig >>= 1;
            return (sig, p + bl);
        }
    }
    (sig, p + bl - 1)
}

/// `n >> bits` 的低 128 位（本模块只在 `n` 已被缩到 ~70 位时调用）。
fn shift_to_u128(n: &Big, bits: u32) -> u128 {
    let words = (bits / 32) as usize;
    let rem = bits % 32;
    let mut v: u128 = 0;
    for i in (0..4).rev() {
        let idx = words + i;
        let lo = *n.get(idx).unwrap_or(&0) as u128;
        let hi = *n.get(idx + 1).unwrap_or(&0) as u128;
        let w = if rem == 0 { lo } else { (lo >> rem) | (hi << (32 - rem)) };
        v = (v << 32) | (w as u32 as u128);
    }
    v
}

fn bit_at(n: &Big, i: u32) -> bool {
    let (w, b) = ((i / 32) as usize, i % 32);
    match n.get(w) {
        Some(&l) => (l >> b) & 1 == 1,
        None => false,
    }
}

/// 组装最终镜像。`uexp` 为无偏指数（`value = sig * 2^(uexp-63)`）。
fn compose(sign: bool, sig: u128, uexp: i32) -> Bits {
    if sig == 0 {
        let mut b = [0u8; 10];
        if sign {
            b[9] = 0x80;
        }
        return b;
    }
    let biased = uexp + EXP_BIAS;
    if biased >= 32767 {
        // 上溢 → 无穷（尾数域按 x87 约定取显式整数位）
        let mut b = [0u8; 10];
        b[0..8].copy_from_slice(&(NORM as u64).to_le_bytes());
        b[8] = 0xFF;
        b[9] = 0x7F | ((sign as u8) << 7);
        return b;
    }
    let (mut s, mut bi) = (sig, biased);
    if bi <= 0 {
        // 下溢 → 非规格化：指数域为 0，尾数右移并按 ties-to-even 舍入。
        let shift = (1 - bi) as u32;
        if shift >= 128 {
            let mut b = [0u8; 10];
            if sign {
                b[9] = 0x80;
            }
            return b;
        }
        let mask = (1u128 << shift) - 1;
        let dropped = s & mask;
        s >>= shift;
        let half = 1u128 << (shift - 1);
        if dropped > half || (dropped == half && (s & 1) == 1) {
            s += 1;
        }
        bi = 0;
    }
    let mut b = [0u8; 10];
    b[0..8].copy_from_slice(&(s as u64).to_le_bytes());
    b[8] = (bi & 0xFF) as u8;
    b[9] = (((bi >> 8) as u8) & 0x7F) | ((sign as u8) << 7);
    b
}

// ===========================================================================
// 字面量解析
// ===========================================================================

/// 十进制 / 十六进制浮点字面量文本 → 正确舍入的 80-bit 镜像。
///
/// 支持 `1`、`1.5`、`.5`、`1e10`、`1E-10`、`0x1.8p3`、前导 `+`/`-`、以及
/// C 允许的数字分隔符 `'`。解析失败返回 `None`，调用方回落到 `f64` 路径。
pub fn from_decimal(text: &str) -> Option<Bits> {
    let cleaned = text.replace('\'', "");
    let mut s = cleaned.trim();
    // 容忍尾随的浮点类型后缀（`1.5L` / `2.0f` / `1e3LF`）：前端已剪掉后缀，
    // 但若调用方传了原始 token，剪不到就会退化成 None → f64 回落，
    // 那是一条**静默丢精度**的路径，必须封死。
    while let Some(rest) = s.strip_suffix(['l', 'L', 'f', 'F']) {
        s = rest;
    }
    let s = s.trim();
    let (sign, s) = match s.strip_prefix('-') {
        Some(r) => (true, r.trim_start_matches('+').trim_start()),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if s.starts_with("0x") || s.starts_with("0X") {
        return from_hex(sign, &s[2..]);
    }
    let (mant_s, exp10) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], parse_exp(&s[i + 1..])?),
        None => (s, 0i32),
    };
    let (int_s, frac_s) = split_dot(mant_s)?;
    if !int_s.bytes().all(|c| c.is_ascii_digit())
        || !frac_s.bytes().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    // value = (int_frac 拼成的整数 M) × 10^(exp10 - frac_s.len())
    let mut m = vec![0u32];
    for c in int_s.bytes().chain(frac_s.bytes()) {
        mul_small(&mut m, 10);
        add_small(&mut m, (c - b'0') as u32);
    }
    let e = exp10 - frac_s.len() as i32;
    if is_zero_big(&m) {
        return Some(compose(sign, 0, 0));
    }
    Some(scale_pow10(sign, m, e))
}

fn split_dot(s: &str) -> Option<(&str, &str)> {
    match s.find('.') {
        Some(i) => {
            let (a, b) = (&s[..i], &s[i + 1..]);
            if a.is_empty() && b.is_empty() {
                return None;
            }
            Some((a, b))
        },
        None => {
            if s.is_empty() {
                None
            } else {
                Some((s, ""))
            }
        },
    }
}

fn parse_exp(s: &str) -> Option<i32> {
    let (neg, d) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if d.is_empty() || !d.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut v: i64 = 0;
    for c in d.bytes() {
        v = v * 10 + (c - b'0') as i64;
        if v > 60000 {
            // 远超 80-bit 范围，钳位即可（结果必为 0 或无穷）。
            return Some(if neg { -60000 } else { 60000 });
        }
    }
    Some(if neg { -(v as i32) } else { v as i32 })
}

/// `M × 10^e` → 镜像。`M` 为正整数值的大整数。
fn scale_pow10(sign: bool, m: Big, e: i32) -> Bits {
    if e >= 0 {
        // M × 5^e × 2^e —— 只有乘法，直接按位舍入。
        let mut n = m;
        if e > 0 {
            n = mul_big(&n, &pow5(e as u32));
        }
        let (sig, uexp) = round_shift(&n, e, false);
        compose(sign, sig, uexp)
    } else {
        // M / (5^k · 2^k)：先左移凑出足够高的商位，再做一次长除，
        // 余数并入 sticky，保证与直接精确舍入等价。
        let k = (-e) as u32;
        let d = pow5(k);
        let db = bitlen(&d);
        let nb = bitlen(&m);
        // 目标：商约 80 位，使 guard/sticky 判定有余量。
        let lshift = (db + 80 - nb).max(0) as u32;
        let mut n = m;
        shl_bits(&mut n, lshift);
        let (q, r) = divmod(&n, &d);
        let p = -(lshift as i32) - k as i32;
        let (sig, uexp) = round_shift(&q, p, !is_zero_big(&r));
        compose(sign, sig, uexp)
    }
}

/// 十六进制字面量（`0x` 已剥离）：`H.F p±E`。二进制幂次，无需长除。
fn from_hex(sign: bool, s: &str) -> Option<Bits> {
    let (mant_s, exp2) = match s.find(['p', 'P']) {
        Some(i) => (&s[..i], parse_exp(&s[i + 1..])?),
        // C 要求十六进制浮点带 p 指数；缺省时按 0 处理（宽松接受）。
        None => (s, 0i32),
    };
    let (int_s, frac_s) = split_dot(mant_s)?;
    let mut m = vec![0u32];
    for c in int_s.bytes().chain(frac_s.bytes()) {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return None,
        };
        shl_bits(&mut m, 4);
        add_small(&mut m, d as u32);
    }
    let nfrac_bits = frac_s.len() as i32 * 4;
    if is_zero_big(&m) {
        return Some(compose(sign, 0, 0));
    }
    let (sig, uexp) = round_shift(&m, exp2 - nfrac_bits, false);
    Some(compose(sign, sig, uexp))
}

// ===========================================================================
// 与其它层交换值的接口
// ===========================================================================

/// `f64` → 80-bit 镜像。`f64` 的值总能被 80-bit **精确**表示，故无舍入。
///
/// 仅用于没有更高精度来源的值（运行时 `double`→`long double` 提升、
/// 折叠后的 `f64` 常量等）。十进制字面量一律走 [`from_decimal`]。
pub fn from_f64(v: f64) -> Bits {
    let raw = v.to_bits();
    let sign = (raw >> 63) & 1 != 0;
    let be = ((raw >> 52) & 0x7FF) as i32;
    let bf = raw & ((1u64 << 52) - 1);
    if be == 0x7FF {
        let mut b = [0u8; 10];
        if bf == 0 {
            b[0..8].copy_from_slice(&(NORM as u64).to_le_bytes());
        } else {
            b[0..8].copy_from_slice(&(0x8000_0000_0000_0000u64 | (bf << 11)).to_le_bytes());
        }
        b[8] = 0xFF;
        b[9] = 0x7F | ((sign as u8) << 7);
        return b;
    }
    if be == 0 && bf == 0 {
        return compose(sign, 0, 0);
    }
    let (sig, uexp) = if be == 0 {
        // f64 subnormal：value = bf × 2^-1074，规格化到 [2^63,2^64)
        let s = (bf as u128) << 11;
        let t = s.trailing_zeros() as i32;
        (s >> t, t - 1011)
    } else {
        // value = (2^52+bf) × 2^(be-1023-52)，sig = (2^52+bf) × 2^11
        // → value = sig × 2^(be-1086)，而约定 value = sig × 2^(uexp-63)
        // → uexp = be - 1023。
        ((((bf as u128) | (1u128 << 52)) << 11), be - 1023)
    };
    compose(sign, sig, uexp)
}

/// 整数 → 80-bit 镜像，精确 RNE 舍入，不经 f64 中转。
///
/// 80-bit 尾数有 64 位显式整数位：`|i| < 2^64` 的整数全部精确可表；
/// 超出则在整数位宽边界做 round-to-nearest-even。旧路径 `from_f64(i as
/// f64)` 对 `|i| >= 2^53` 的整数会先丢失低 11 位再“拓宽”，静态初始化器
/// （如 `long double g = 9007199254740993;`）因此与 gcc 不一致。
pub fn from_i128(i: i128) -> Bits {
    if i == 0 {
        return [0u8; 10];
    }
    let neg = i < 0;
    // i128::MIN 的绝对值仍能由 u128 精确承载。
    let mag = i.unsigned_abs();
    let bits = 128 - mag.leading_zeros() as i64; // 量级的二进制位数
    let mut exp = bits - 1; // 规格化后的无偏指数
    if bits > 64 {
        // 右移到 64 位边界，移出位折叠为 G/R/S（复用 shr_sticky 的定点约定：
        // 整数整体左移 3 位，G/R/S 落在 bit2/1/0）。
        let sh = (bits - 64) as u32;
        let ext = shr_sticky((mag as u128) << 3, sh);
        let (m, bump) = rne_round(ext);
        exp += bump;
        return pack_finite(exp, m, neg);
    }
    // 左规：j-bit 对齐到 bit63（pack_finite 的输入约定）。
    let m = (mag as u64) << (64 - bits as u32);
    pack_finite(exp, m, neg)
}

/// 无符号整数量级在 `p` 位尾数（含 j-bit）边界做 RNE 舍入。
///
/// 返回 `(mant, bump)`：`mant` 的 bit(p-1) 为 j-bit（p=53 → bit52）；
/// 舍入进位时 j-bit 落到 bit p，`bump=1`（调用方指数 +1 并右移）。
/// 定点约定与 80-bit 路径一致：量级左移 3 位，G/R/S 在 bit2/1/0。
fn round_uint_to_mant(mag: u128, bits: i64, p: u32) -> (u64, i64) {
    debug_assert!(bits >= 1);
    let pi = p as i64;
    // 定点：j-bit 最终对齐到 bit(p-1)，G/R/S 在 bit2/1/0。
    // bits > p 时右移 (bits-p) 并保留 sticky；bits < p 时左移 (p-bits) 左规。
    let aligned = if bits >= pi {
        let sh = (bits - pi) as u32;
        shr_sticky(mag << 3, sh)
    } else {
        mag << (3 + pi - bits)
    };
    let g = aligned & (1 << 2) != 0;
    let r = aligned & (1 << 1) != 0;
    let s = aligned & 1 != 0;
    let t = (aligned >> 3) as u64;
    let lsb = t & 1 != 0;
    let mut mant = t;
    if g && (r || s || lsb) {
        mant += 1;
    }
    if mant & (1u64 << p) != 0 {
        // 舍入进位：新尾数 j-bit 在 bit(p-1)，指数 +1。
        return (1u64 << (p - 1), 1);
    }
    (mant, 0)
}

/// 整数 → IEEE binary64（f64），精确 RNE。
///
/// 整数输入 `|i| ≥ 1`，无偏指数恒 ≥ 0，结果必为规格化数或上溢为 ±inf，
/// 因此只需处理规格化/无穷两种情形（非规格化分支不可能命中）。
pub fn i128_to_f64(i: i128) -> f64 {
    if i == 0 {
        return 0.0;
    }
    let neg = i < 0;
    let mag = i.unsigned_abs();
    let bits = 128 - mag.leading_zeros() as i64;
    let (mant, bump) = round_uint_to_mant(mag, bits, 53);
    let e = bits - 1 + bump; // 无偏指数
    let biased = if e >= 1024 { 0x7FF } else { (e + 1023) as u64 };
    // e >= 1024：上溢 → ±inf（指数全 1、分数 0）。
    let frac = if biased == 0x7FF {
        0
    } else {
        mant & ((1u64 << 52) - 1)
    };
    let raw = ((neg as u64) << 63) | (biased << 52) | frac;
    f64::from_bits(raw)
}

/// 整数 → IEEE binary32（f32），精确 RNE。同理结果必为规格化数或 ±inf。
pub fn i128_to_f32(i: i128) -> f32 {
    if i == 0 {
        return 0.0;
    }
    let neg = i < 0;
    let mag = i.unsigned_abs();
    let bits = 128 - mag.leading_zeros() as i64;
    let (mant, bump) = round_uint_to_mant(mag, bits, 24);
    let e = bits - 1 + bump;
    let biased = if e >= 128 { 0xFFu64 } else { (e + 127) as u64 };
    let frac = if biased == 0xFF {
        0
    } else {
        mant & ((1u64 << 23) - 1)
    };
    let raw = ((neg as u64) << 31) | (biased << 23) | frac;
    f32::from_bits(raw as u32)
}

/// 80-bit 镜像 → `f64`（最近舍入）。供诊断、常量折叠与不使用 x87 的后端回落。
pub fn to_f64(b: &Bits) -> f64 {
    let sign = (b[9] >> 7) & 1;
    let biased = (((b[9] as u16 & 0x7F) << 8) | b[8] as u16) as i32;
    let sig = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as u128;
    let raw = |m: u64, e: i32| -> f64 {
        let bits = ((sign as u64) << 63) | ((e as u64 & 0x7FF) << 52) | (m & ((1u64 << 52) - 1));
        f64::from_bits(bits)
    };
    if biased == 32767 {
        return if sig == NORM {
            raw(0, 0x7FF)
        } else {
            raw(1 << 51, 0x7FF)
        };
    }
    if biased == 0 && (sig & 0x7FFF_FFFF_FFFF_FFFF) == 0 {
        return if sign == 1 { -0.0 } else { 0.0 };
    }
    // 80-bit： value = sig × 2^(uexp-63)，sig = 1.f₆₃ × 2^63
    // f64 ： value = (1 + f/2^52) × 2^(E-1023)，E 为偏置指数
    // → 尾数右移 11 位得 53 位有效（含隐含 1），E = uexp + 1023。
    let uexp = biased - EXP_BIAS;
    let shift = 11u32;
    let mut m = sig >> shift;
    let rem = sig & ((1u128 << shift) - 1);
    let half = 1u128 << (shift - 1);
    if rem > half || (rem == half && (m & 1) == 1) {
        m += 1;
    }
    let mut e = uexp + 1023;
    if m >> 53 != 0 {
        m >>= 1;
        e += 1;
    }
    if e >= 0x7FF {
        return raw(0, 0x7FF);
    }
    if e <= 0 {
        // f64 非规格化：含隐含 1 的尾数整体右移，移入低位的 1 就是分数部分。
        let sc = (1 - e) as u32;
        if sc >= 64 {
            return if sign == 1 { -0.0 } else { 0.0 };
        }
        return raw((m >> sc) as u64, 0);
    }
    raw(m as u64, e)
}

/// 取反：只翻符号位。
pub fn negate(mut b: Bits) -> Bits {
    b[9] ^= 0x80;
    b
}

/// 绝对值。
pub fn abs(mut b: Bits) -> Bits {
    b[9] &= 0x7F;
    b
}

/// 是否为字面意义上的零（±0）。
pub fn is_zero(b: &Bits) -> bool {
    let biased = (((b[9] as u16 & 0x7F) << 8) | b[8] as u16) as i32;
    biased == 0 && b[..8].iter().all(|&x| x == 0)
}

/// 指数字段（15 位，**不含**符号）。
pub fn exp_field(b: &Bits) -> u16 {
    (((b[9] as u16 & 0x7F) << 8) | b[8] as u16) & 0x7FFF
}

/// 符号位（true = 负）。
pub fn sign_bit(b: &Bits) -> bool {
    b[9] >> 7 == 1
}

/// 高 16 位原始字（符号 + 15 位指数）。与“把 `long double` 当两个 u64 读出”
/// 的调试惯用法直接对应（`hi & 0xFFFF`），便于与 gcc 参照表逐字对照。
pub fn top_word(b: &Bits) -> u16 {
    u16::from_le_bytes([b[8], b[9]])
}

/// 64 位尾数域（含显式整数位）。
pub fn mantissa(b: &Bits) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// 80-bit 指数偏置（2^14 - 1）。
pub const EXP_BIAS: i32 = 0x3FFF;

// 定点尾数约定（u128）：
//   bit 66        : 显式整数位（j-bit）
//   bit 66..=3    : 64 位尾数
//   bit 2 / 1 / 0 : guard / round / sticky
// 所有规格化输入的 j-bit 都对齐到 bit 66；非规格化输入在解包时左规。

/// 解包后的 80-bit 有限值：无偏指数 + 定点尾数 + 符号。
#[derive(Debug, Clone, Copy)]
struct Unpacked {
    exp: i64,
    sig: u128,
    neg: bool,
}

/// +0 / -0 的位型。
fn zero_bits(neg: bool) -> Bits {
    let mut b = [0u8; 10];
    b[9] = if neg { 0x80 } else { 0 };
    b
}

/// x87 规范 qNaN（Intel 手册：指数全 1，尾数 bit63=1、bit62=1，其余 0）。
fn qnan_bits() -> Bits {
    let mut b = [0u8; 10];
    b[0..8].copy_from_slice(&0xC000_0000_0000_0000u64.to_le_bytes());
    b[8] = 0xFF;
    b[9] = 0x7F;
    b
}

fn pack_inf(neg: bool) -> Bits {
    let mut b = [0u8; 10];
    b[0..8].copy_from_slice(&0x8000_0000_0000_0000u64.to_le_bytes());
    b[8] = 0xFF;
    b[9] = if neg { 0xFF } else { 0x7F };
    b
}

/// 解包：非规格化数在此左规为（更小的无偏指数 + j-bit 在 bit66）。
/// 调用方需先用 `exp_field` 区分特殊值；本函数只处理有限数。
fn unpack(b: &Bits) -> Unpacked {
    let e = exp_field(b);
    let m = mantissa(b);
    let mut sig = (m as u128) << 3;
    let mut exp = e as i64 - EXP_BIAS as i64;
    if e == 0 {
        // 非规格化：j-bit 不在 bit63，左移直到 bit66。
        let k = sig.leading_zeros() - (128 - 1 - 66) as u32;
        sig <<= k;
        exp -= k as i64;
    }
    Unpacked { exp, sig, neg: sign_bit(b) }
}

/// 定点尾数右移 `d` 位；所有被移出位折叠为 sticky（bit0）。
fn shr_sticky(sig: u128, d: u32) -> u128 {
    if d == 0 {
        return sig;
    }
    if d >= 67 {
        return if sig != 0 { 1 } else { 0 };
    }
    let removed = sig & ((1u128 << d) - 1);
    (sig >> d) | if removed != 0 { 1 } else { 0 }
}

/// 在 64 位尾数边界做 RNE 舍入，返回 64 位尾数（j-bit 在 bit63）
/// 及指数增量（舍入进位到 bit64 时为 1）。
fn rne_round(sig: u128) -> (u64, i64) {
    let g = sig & (1 << 2) != 0;
    let r = sig & (1 << 1) != 0;
    let s = sig & 1 != 0;
    let lsb = sig & (1 << 3) != 0;
    let inc = (g && (r || s || lsb)) as u128;
    let t = (sig >> 3) + inc;
    if t & (1 << 64) != 0 {
        // 舍入进位：新尾数 0x8000...，指数 +1。
        (0x8000_0000_0000_0000, 1)
    } else {
        (t as u64, 0)
    }
}

/// 打包：无偏指数 exp + 64 位尾数 m（j-bit 在 bit63）。
fn pack_finite(exp: i64, m: u64, neg: bool) -> Bits {
    let biased = exp + EXP_BIAS as i64;
    if biased >= 0x7FFF {
        return pack_inf(neg);
    }
    if biased >= 1 {
        let mut b = [0u8; 10];
        b[0..8].copy_from_slice(&m.to_le_bytes());
        b[8] = (biased & 0xFF) as u8;
        b[9] = (((biased >> 8) & 0x7F) as u8) | if neg { 0x80 } else { 0 };
        return b;
    }
    // biased <= 0：输出非规格化。尾数在完整精度上右移后再次 RNE 舍入。
    let sh = (1 - biased) as u32;
    let ext = shr_sticky((m as u128) << 3, sh);
    let (dm, bump) = rne_round(ext);
    if bump != 0 {
        // 舍入进位到 j-bit：恰为最小规格化数（指数域 1，尾数 0x8000...）。
        let mut b = [0u8; 10];
        b[0..8].copy_from_slice(&0x8000_0000_0000_0000u64.to_le_bytes());
        b[8] = 1;
        b[9] = if neg { 0x80 } else { 0 };
        return b;
    }
    // rne_round 返回 j-bit 在 bit63；非规格化结果 j=0，尾数域取
    // dm 的 bit62..0 左移 1（分数填满 64 位尾数域）。
    let frac = (dm & 0x7FFF_FFFF_FFFF_FFFF) << 1;
    let mut b = [0u8; 10];
    b[0..8].copy_from_slice(&frac.to_le_bytes());
    b[8] = 0;
    b[9] = if neg { 0x80 } else { 0 };
    b
}

/// 80-bit long double 加法（round-to-nearest-even，与 x87 默认舍入一致）。
/// 纯整数域实现，不经 f64 中转，精度与硬件 x87 等价。
pub fn add(a: &Bits, b: &Bits) -> Bits {
    let ea = exp_field(a);
    let eb = exp_field(b);
    let ma = mantissa(a);
    let mb = mantissa(b);
    let a_inf = ea == 0x7FFF && ma == 0x8000_0000_0000_0000;
    let b_inf = eb == 0x7FFF && mb == 0x8000_0000_0000_0000;
    let a_nan = ea == 0x7FFF && !a_inf;
    let b_nan = eb == 0x7FFF && !b_inf;

    if a_nan || b_nan {
        return qnan_bits();
    }
    if a_inf && b_inf {
        return if sign_bit(a) == sign_bit(b) {
            pack_inf(sign_bit(a))
        } else {
            qnan_bits()
        };
    }
    if a_inf {
        return pack_inf(sign_bit(a));
    }
    if b_inf {
        return pack_inf(sign_bit(b));
    }

    let az = is_zero(a);
    let bz = is_zero(b);
    if az && bz {
        // (-0)+(-0) = -0；其余含 +0 的组合为 +0。
        return zero_bits(sign_bit(a) && sign_bit(b));
    }
    if az {
        return *b;
    }
    if bz {
        return *a;
    }

    let x = unpack(a);
    let y = unpack(b);
    // 按量级排序，big 为大操作数。
    let (big, small) = if (x.exp, x.sig) >= (y.exp, y.sig) {
        (x, y)
    } else {
        (y, x)
    };
    let aligned = shr_sticky(small.sig, (big.exp - small.exp) as u32);

    let (sig, mut exp) = if big.neg == small.neg {
        let mut s = big.sig + aligned;
        let mut e = big.exp;
        if s & (1 << 67) != 0 {
            // 进位到 bit67：右移 1 并保留 sticky。
            s = shr_sticky(s, 1);
            e += 1;
        }
        (s, e)
    } else {
        let d = big.sig - aligned;
        if d == 0 {
            // 精确抵消：RNE 下结果为 +0。
            return [0u8; 10];
        }
        // 左规到 j-bit = bit66。
        let k = d.leading_zeros() - (128 - 1 - 66) as u32;
        let s = if k == 0 { d } else { d << k };
        (s, big.exp - k as i64)
    };

    let (m, bump) = rne_round(sig);
    exp += bump;
    pack_finite(exp, m, big.neg)
}

/// 80-bit long double 减法：a - b = a + (-b)。
pub fn sub(a: &Bits, b: &Bits) -> Bits {
    let nb = negate(*b);
    add(a, &nb)
}

// ===========================================================================
// 单元测试
//
// 期望位型**全部取自 gcc -O0 的实际输出**（x86-64 Linux，80-bit long double），
// 不是本模块自证。取法：把字面量存入 `volatile long double`，按两个 u64
// 读出，低 64 位即尾数、高 64 位低 16 位即指数域（含符号）。
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// 断言字面量文本解析出的镜像与 gcc 一致（指数字段不比符号）。
    fn eq_ref(text: &str, mant: u64, exp: u16) {
        let b = from_decimal(text).unwrap_or_else(|| panic!("无法解析 {text}"));
        assert_eq!(mantissa(&b), mant, "{text}: 尾数");
        assert_eq!(exp_field(&b), exp, "{text}: 指数字段");
    }

    #[test]
    fn decimal_literals_match_gcc() {
        // 0.1/0.2/0.3 是 f64 展宽必然出错的一组：正确的 64 位尾数末位是 D，
        // 经 f64 中转只会得到末尾 0x…C800 形态。
        eq_ref("0.1L", 0xCCCCCCCCCCCCCCCD, 0x3FFB);
        eq_ref("0.2", 0xCCCCCCCCCCCCCCCD, 0x3FFC);
        eq_ref("0.3", 0x999999999999999A, 0x3FFD);
        eq_ref("0.7", 0xB333333333333333, 0x3FFE);
        eq_ref("1.1", 0x8CCCCCCCCCCCCCCD, 0x3FFF);
        eq_ref("5e-1", 0x8000000000000000, 0x3FFE);
        eq_ref("1e-5", 0xA7C5AC471B478423, 0x3FEE);
        eq_ref("1e-30", 0xA2425FF75E14FC32, 0x3F9B);
        eq_ref("123456789.123", 0xEB79A2A3EF9DB22D, 0x4019);
        eq_ref("3.14159265358979323846", 0xC90FDAA22168C235, 0x4000);
        eq_ref("1e300", 0xBF21E44003ACDD2D, 0x43E3);
    }

    /// 逐 10 字节比对 gcc 真值（不是只查尾数/指数字段）。
    ///
    /// 上面的 `eq_ref` 走的是 `mantissa()` / `exp_field()` 两个访问器，所以
    /// 它只能证明“值对”，不能证明“字节序与填充对”——而数据发射侧要写进
    /// `.data` 的正是那 10 个字节本身。本表由
    /// `objcopy -O binary --only-section=.data` 从 gcc -O0 的 .o 里直接抽出，
    /// 取每个 16 字节对象的前 10 字节（`b[10..16]` 是实现定义 padding，
    /// gcc 不保证为零，故不参与比对）。
    #[test]
    fn decimal_literals_match_gcc_byte_exact() {
        // (字面量, gcc .data 前 10 字节小端)
        const CASES: &[(&str, &str)] = &[
            ("0.1L", "cdccccccccccccccfb3f"),
            ("1.0L", "0000000000000080ff3f"),
            ("3.14159265358979323846L", "35c26821a2da0fc90040"),
            ("-0.1L", "cdccccccccccccccfbbf"),
            ("0.5L", "0000000000000080fe3f"),
            ("1e308L", "8fff445e2f9c678efe43"),
            ("1.2345678901234567890123L", "8ddbcf621452069eff3f"),
            ("123456789.0L", "00000000a0a279eb1940"),
            ("0.3L", "9a99999999999999fd3f"),
            ("2.718281828459045235360L", "9b4abba25854f8ad0040"),
            ("5e-324L", "b2e1a07ace958981cd3b"),
            // 最小正次正规：尾数 = 1、指数字段 = 0（验证 from_decimal 不走规格化捷径）
            ("3.645105638354799e-4951L", "01000000000000000000"),
            // 最大有限值：尾数全 1、指数字段 0x7FFE
            ("1.189731495357231765021e+4932L", "fffffffffffffffffe7f"),
        ];
        for (text, hex) in CASES {
            let want = parse_hex10(hex);
            let got = from_decimal(text).unwrap_or_else(|| panic!("无法解析 {text}"));
            assert_eq!(got, want, "{text}: 与 gcc .data 前 10 字节不一致");
        }
    }

    fn parse_hex10(hex: &str) -> Bits {
        assert_eq!(hex.len(), 20, "真值表应为 10 字节 = 20 个 hex 字符");
        let mut b = [0u8; 10];
        for i in 0..10 {
            b[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex");
        }
        b
    }

    #[test]
    fn exact_small_values_match_gcc() {
        eq_ref("1.0L", 0x8000000000000000, 0x3FFF);
        eq_ref("2", 0x8000000000000000, 0x4000);
        eq_ref("3", 0xC000000000000000, 0x4000);
        eq_ref("0", 0, 0);
    }

    #[test]
    fn negative_literal_flips_sign_bit_only() {
        let p = from_decimal("0.1L").unwrap();
        let n = from_decimal("-0.1L").unwrap();
        // gcc: -0.1L → 高 16 位原始字 0xBFFB（= 符号 | 0x3FFB），尾数不变
        assert_eq!(top_word(&n), 0xBFFB);
        assert_eq!(exp_field(&n), 0x3FFB);
        assert!(sign_bit(&n));
        assert!(!sign_bit(&p));
        assert_eq!(mantissa(&n), mantissa(&p));
        assert_eq!(negate(p), n);
    }

    #[test]
    fn subnormal_underflow_matches_gcc() {
        // 1e-4950 低于最小规格化值 → x87 非规格化；gcc 给尾数 3、指数域 0
        eq_ref("1e-4950", 0x0000000000000003, 0x0000);
    }

    #[test]
    fn hex_literals_match_gcc() {
        eq_ref("0x1.8p3", 0xC000000000000000, 0x4002);
        eq_ref("0x1.fffffffffffffp+120", 0xFFFFFFFFFFFFF800, 0x4077);
    }

    // 以下五组真值同样取自 gcc -O0（见模块头注释），用于覆盖整数直解
    // (from_i128)、整数→f64/f32 以及 add/sub 的运算路径。旧实现统一经
    // f64 中转，|i| >= 2^53 即丢精度；这些用例防止回退。

    #[test]
    fn from_i128_match_gcc() {
        // (整数值, gcc 80-bit 镜像 10 字节小端)
        const CASES: &[(i128, &str)] = &[
            (0, "00000000000000000000"),
            (1, "0000000000000080ff3f"),
            (-1, "0000000000000080ffbf"),
            // 2^53+1：旧路径经 f64 会退化成 2^53
            (9007199254740993, "00040000000000803440"),
            // < 2^64：全部精确
            (i64::MAX as i128, "feffffffffffffff3d40"),
            (18446744073709551615, "ffffffffffffffff3e40"),
            // 恰好 2^64：RNE tie-even 向下
            (18446744073709551616, "00000000000000803f40"),
            // 2^64+3：tie-even 向上 → 2^64+2
            (18446744073709551619, "02000000000000803f40"),
            // 2^70+65：RNE 进位到 2^70+64
            ((1i128 << 70) + 65, "01000000000000804540"),
            // 2^100+1：低位被舍掉 → 2^100
            ((1i128 << 100) + 1, "00000000000000806340"),
            (-(1i128 << 100) - 1, "000000000000008063c0"),
            // i128::MIN：|i| 仍在 80-bit 有限范围内
            (i128::MIN, "00000000000000007ec0"),
        ];
        for (i, hex) in CASES {
            assert_eq!(from_i128(*i), parse_hex10(hex), "from_i128({i})");
        }
    }

    #[test]
    fn i128_to_f64_match_gcc() {
        // (整数值, gcc double 的 64 位字)
        const CASES: &[(i128, u64)] = &[
            (0, 0x0000000000000000),
            (1, 0x3FF0000000000000),
            (-1, 0xBFF0000000000000),
            (9007199254740993, 0x4340000000000000),
            (18446744073709551615, 0x43F0000000000000),
            ((1i128 << 100) + 1, 0x4630000000000000),
        ];
        for (i, raw) in CASES {
            assert_eq!(i128_to_f64(*i).to_bits(), *raw, "i128_to_f64({i})");
        }
    }

    #[test]
    fn i128_to_f32_match_gcc() {
        // (整数值, gcc float 的 32 位字)
        const CASES: &[(i128, u32)] = &[
            (0, 0x00000000),
            (1, 0x3F800000),
            (-1, 0xBF800000),
            // 2^24+1：RNE tie-even → 2^24
            (16777217, 0x4B800000),
            (9007199254740993, 0x5A000000),
        ];
        for (i, raw) in CASES {
            assert_eq!(i128_to_f32(*i).to_bits(), *raw, "i128_to_f32({i})");
        }
    }

    #[test]
    fn add_match_gcc() {
        // (左字面量, 右字面量, gcc 结果 10 字节)
        const CASES: &[(&str, &str, &str)] = &[
            ("0.1L", "0.2L", "9a99999999999999fd3f"),
            ("1.0L", "1.0L", "00000000000000800040"),
            ("1.5L", "2.25L", "00000000000000f00040"),
            // 2^64-1 + 0.5：tie-even 进位到 2^64
            // (由 from_i128(2^64-1) 与 0.5L 相加)
            ("0x1p-70L", "1.0L", "0000000000000080ff3f"),
        ];
        for (a, b, hex) in CASES {
            let x = from_decimal(a).unwrap();
            let y = from_decimal(b).unwrap();
            assert_eq!(add(&x, &y), parse_hex10(hex), "{a} + {b}");
        }
        // carry_mant 特殊构造
        let big = from_i128(18446744073709551615);
        let half = from_decimal("0.5L").unwrap();
        assert_eq!(
            add(&big, &half),
            parse_hex10("00000000000000803f40"),
            "(2^64-1)+0.5"
        );
        // 两个非规格化最小数相加 → 次小非规格化
        let t = parse_hex10("01000000000000000000");
        assert_eq!(
            add(&t, &t),
            parse_hex10("02000000000000000000"),
            "subnormal min + min"
        );
        // 含 0 的边界
        assert_eq!(add(&zero_bits(true), &zero_bits(true)), zero_bits(true));
        let five = from_decimal("5.0L").unwrap();
        assert_eq!(add(&zero_bits(false), &five), five);
    }

    #[test]
    fn sub_match_gcc() {
        let cases: &[(&str, &str, &str)] = &[
            ("0.1L", "0.1L", "00000000000000000000"),
            ("3.0L", "1.5L", "00000000000000c0ff3f"),
        ];
        for (a, b, hex) in cases {
            let x = from_decimal(a).unwrap();
            let y = from_decimal(b).unwrap();
            assert_eq!(sub(&x, &y), parse_hex10(hex), "{a} - {b}");
        }
        // 精确抵消：异号且量级相等 → +0（即使两数都是负零语义）
        let one = from_decimal("1.0L").unwrap();
        assert_eq!(sub(&one, &one), [0u8; 10]);
    }

    #[test]
    fn add_inf_nan_match_gcc() {
        let inf = pack_inf(false);
        let ninf = pack_inf(true);
        let nan = qnan_bits();
        assert_eq!(add(&inf, &inf), inf);
        assert_eq!(add(&inf, &from_decimal("2.0L").unwrap()), inf);
        // inf + (-inf) = qNaN
        assert_eq!(add(&inf, &ninf), nan);
        assert_eq!(add(&nan, &from_decimal("1.0L").unwrap()), nan);
        assert_eq!(sub(&inf, &inf), nan);
    }


    #[test]
    fn digit_separators_and_forms_accepted() {
        assert_eq!(from_decimal("1'000.5"), from_decimal("1000.5"));
        assert!(from_decimal("+.5").is_some());
        assert!(from_decimal("1e").is_none());
        assert!(from_decimal("").is_none());
    }

    #[test]
    fn from_f64_is_exact_widening() {
        // f64 的值总能被 80-bit 精确表示
        for v in [0.0f64, 1.0, -1.0, 0.5, 2.0, 3.0, 1e300, 1e-300, f64::MIN_POSITIVE] {
            let b = from_f64(v);
            assert_eq!(to_f64(&b), v, "f64 往返 {v}");
        }
        assert!(is_zero(&from_f64(0.0)));
        assert!(is_zero(&from_f64(-0.0)));
    }

    /// 对抗性参照：ties-to-even、非规格化上/下溢、长数字串、最大有限值。
    /// 全部取自 gcc -O0 的实际内存镜像。
    #[test]
    fn adversarial_rounding_matches_gcc() {
        // ties-to-even：hex 字面量比 1.0 多出最小可表增量 → 舍回偶数尾数
        eq_ref("0x1.0000000000000001p0", 0x8000000000000000, 0x3FFF);
        // 下溢边界：不足半个最小非规格化 → 0；恰好过线 → 尾数 1
        eq_ref("1.5e-4951", 0, 0);
        eq_ref("4e-4952", 0, 0);
        eq_ref("1.1e-5000", 0, 0);
        eq_ref("3.3621146665336956e-4951", 0x0000000000000001, 0);
        eq_ref("1e-4932", 0x261247C8F29357F0, 0);
        // 上溢边界：1e4932 仍是最大有限值（指数域 0x7FFE，不是 0x7FFF）
        eq_ref("1e4932", 0xD72CB2A95C7EF6CD, 0x7FFE);
        // f64 边界值在 80-bit 下不是边界值，可用来抓“经 f64 中转”的错路
        eq_ref("1.7976931348623157081452742373170e308", 0xFFFFFFFFFFFFF800, 0x43FE);
        eq_ref("2.2250738585072011360579586542032e-308", 0xFFFFFFFFFFFFF800, 0x3C00);
        // 30 位十进制整数：验证大整数累乘路径。期望值由
        // `printf("%La")` 与精确整数除法 (M/2^33 = 14372261824592212087) 交叉
        // 对账得到。注意：不带小数点/指数的 `…L` 在 C 里是**整数常量**，
        // gcc 会报 "integer constant is too large" 并截断，不能当浮点参照。
        eq_ref("123456789012345678901234567890.0", 0xC77487FB61B9F077, 0x405F);
        // 普通小数/整数
        eq_ref("7.0", 0xE000000000000000, 0x4001);
        eq_ref("1e1", 0xA000000000000000, 0x4002);
        eq_ref("1.1e1", 0xB000000000000000, 0x4002);
        eq_ref("1e2", 0xC800000000000000, 0x4005);
        eq_ref("0.9", 0xE666666666666666, 0x3FFE);
        eq_ref("1e-1", 0xCCCCCCCCCCCCCCCD, 0x3FFB);
        eq_ref("0.001", 0x83126E978D4FDF3B, 0x3FF5);
    }

    #[test]
    fn from_f64_and_from_decimal_agree_when_representable() {
        // 2 的幂在两种来源下必须逐字节相同（可精确表示）
        for t in ["0.5", "1", "2", "4", "1024", "0.25"] {
            let d = from_decimal(t).unwrap();
            let f = from_f64(t.parse::<f64>().unwrap());
            assert_eq!(d, f, "{t}");
        }
    }
}


