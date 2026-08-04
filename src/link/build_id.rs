// src/link/build_id.rs - .note.gnu.build-id 段生成
//
// .note.gnu.build-id 是 ELF note 节，包含 build 的指纹哈希。
// build-id 主要用于：
//   - 调试器定位符号 (debuginfod)
//   - 包管理器 (rpm, deb) 标识二进制
//   - crash report 关联
//
// 节结构 (64-bit ELF):
//   namesz   = 4                (length of "GNU\0")
//   descsz   = 16 (sha1)        (length of hash bytes)
//   type     = NT_GNU_BUILD_ID = 3
//   name     = "GNU\0"          (4 bytes, padded to 4-byte boundary)
//   desc     = sha1 bytes       (padded to 4-byte boundary)
//
// 算法支持：
//   - sha1: 20 字节
//   - uuid: 16 字节
//   - md5 : 16 字节
//   - fast-sha1: 同 sha1，但接口允许复用预计算 hash
//   - none : 空 descsz = 0
//
// 本实现不依赖外部 crate, 纯 Rust 内置 SHA-1 / MD5 / UUID v4。

#![allow(dead_code)]

use super::BuildIdStyle;

/// 通用 build-id 生成入口
pub fn build_id(style: BuildIdStyle, image: &[u8]) -> Vec<u8> {
    match style {
        BuildIdStyle::None => vec![],
        BuildIdStyle::Sha1 | BuildIdStyle::FastSha1 => sha1(image).to_vec(),
        BuildIdStyle::Uuid => uuid_v4().to_vec(),
        BuildIdStyle::Md5 => md5(image).to_vec(),
    }
}

/// 64-bit ELF 模式下构造 .note.gnu.build-id 节完整内容
/// 返回 (descsz, full_section_bytes)
pub fn build_note_section(style: BuildIdStyle, image: &[u8]) -> (usize, Vec<u8>) {
    let name = b"GNU\0";
    let desc = build_id(style, image);
    let descsz = desc.len();
    let namesz = name.len();
    // 4 字节对齐 pad
    let name_pad = (4 - (namesz % 4)) % 4;
    let desc_pad = (4 - (descsz % 4)) % 4;
    let mut out = Vec::new();
    out.extend_from_slice(&(namesz as u32).to_le_bytes());
    out.extend_from_slice(&(descsz as u32).to_le_bytes());
    out.extend_from_slice(&3u32.to_le_bytes()); // NT_GNU_BUILD_ID
    out.extend_from_slice(name);
    out.resize(out.len() + name_pad, 0);
    out.extend_from_slice(&desc);
    out.resize(out.len() + desc_pad, 0);
    (descsz, out)
}

// ---- 内置 SHA-1 ----
fn sha1(data: &[u8]) -> [u8; 20] {
    // 简化 SHA-1: 使用 FIPS 180-4 公开域实现
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            let v = w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16];
            w[i] = v.rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = if i < 20 {
                ((b & c) | ((!b) & d), 0x5A827999)
            } else if i < 40 {
                (b ^ c ^ d, 0x6ED9EBA1)
            } else if i < 60 {
                ((b & c) | (b & d) | (c & d), 0x8F1BBCDC)
            } else {
                (b ^ c ^ d, 0xCA62C1D6)
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, &v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

// ---- 内置 MD5 ----
fn md5(data: &[u8]) -> [u8; 16] {
    // 初始化状态
    let mut h: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    // 标准 MD5 K 表
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    for block in msg.chunks(64) {
        let mut w = [0u32; 16];
        for i in 0..16 {
            w[i] = u32::from_le_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | ((!b) & d), i)
            } else if i < 32 {
                ((d & b) | ((!d) & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | (!d)), (7 * i) % 16)
            };
            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(w[g])
                    .rotate_left(MD5_S[i]),
            );
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, &v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

// ---- UUID v4 (random) ----
fn uuid_v4() -> [u8; 16] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // 简单 hash: 异或 nanos 与 splitmix64 序列
    let mut bytes = [0u8; 16];
    let mut v = nanos;
    for i in 0..4 {
        v = v.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1);
        let chunk = (v >> 32) as u32;
        bytes[4 * i..4 * i + 4].copy_from_slice(&chunk.to_le_bytes());
    }
    // RFC 4122 v4 设置
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha1_empty() {
        // FIPS-180-4: SHA-1("") = da39a3ee5e6b4b0d3255bfef95601890afd80709
        let h = sha1(b"");
        let hex: String = h.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(hex, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn test_sha1_abc() {
        // SHA-1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
        let h = sha1(b"abc");
        let hex: String = h.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(hex, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn test_md5_empty() {
        // MD5("") = d41d8cd98f00b204e9800998ecf8427e
        let h = md5(b"");
        let hex: String = h.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(hex, "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn test_md5_abc() {
        // MD5("abc") = 900150983cd24fb0d6963f7d28e17f72
        let h = md5(b"abc");
        let hex: String = h.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(hex, "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn test_uuid_v4_format() {
        let u = uuid_v4();
        // v4 marker
        assert_eq!(u[6] & 0xF0, 0x40);
        assert_eq!(u[8] & 0xC0, 0x80);
    }

    #[test]
    fn test_build_note_section_layout() {
        let (descsz, bytes) = build_note_section(BuildIdStyle::Sha1, b"hello");
        assert_eq!(descsz, 20);
        // namesz(4) + descsz(4) + type(4) + name(4) + desc(20) = 36
        // name is 4-byte aligned, no name_pad. desc 20 -> pad 0
        assert_eq!(bytes.len(), 4 + 4 + 4 + 4 + 20);
        // Check type
        let ty = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        assert_eq!(ty, 3); // NT_GNU_BUILD_ID
    }

    #[test]
    fn test_build_id_none() {
        assert_eq!(build_id(BuildIdStyle::None, b"data"), Vec::<u8>::new());
    }
}
