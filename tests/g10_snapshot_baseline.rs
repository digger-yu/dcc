//! G10 快照门禁: 步骤 0.
//!
//! 第一性原理: x86_64 后端每一步"把发射助手从手写字节改成构造类型化枚举"都必须
//! **逐字节保持 .text 输出不变**. 本测试对每个选定 fixture 用 `dcc -c` 编译出 .o,
//! 抽出 `.text` 节数据, 算 sha256, 与 `tests/fixtures/g10_<name>.sha256` 里落盘的基线比对.
//!
//! 首次运行: 基线文件不存在时生成并落盘 (打印 "baseline generated").
//! 之后: 基线存在则必须全等, 否则测试失败.
//!
//! 为什么不用外部 sha2 依赖: dcc 是 suckless 风格, Cargo.toml 无 sha2 crate;
//! 本测试自带一份与 RFC 6234 一致的最小 sha256 实现, 避免为测试引入依赖.

// ---- minimal sha256 (RFC 6234) ----
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bitlen = (data.len() as u64).wrapping_mul(8);
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    sha256(data).iter().map(|b| format!("{:02x}", b)).collect()
}

// ---- ELF64 .text extraction (mirrors tests/common/harness/elf.rs minimal slice) ----
fn elf64_text<'a>(data: &'a [u8]) -> Option<&'a [u8]> {
    fn read_cstr(b: &[u8]) -> String {
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).into_owned()
    }
    if data.len() < 64 {
        return None;
    }
    let e_shoff = u64::from_le_bytes(data[40..48].try_into().ok()?) as usize;
    let e_shentsize = u16::from_le_bytes(data[58..60].try_into().ok()?) as usize;
    let e_shnum = u16::from_le_bytes(data[60..62].try_into().ok()?) as usize;
    let e_shstrndx = u16::from_le_bytes(data[62..64].try_into().ok()?) as usize;
    if e_shoff + e_shentsize * e_shnum > data.len() {
        return None;
    }
    let shstr = e_shoff + e_shstrndx as usize * e_shentsize;
    let shstr_off = u64::from_le_bytes(data[shstr + 24..shstr + 32].try_into().ok()?) as usize;
    for i in 0..e_shnum {
        let off = e_shoff + i * e_shentsize;
        let name_off = u32::from_le_bytes(data[off..off + 4].try_into().ok()?) as usize;
        if shstr_off + name_off >= data.len() {
            continue;
        }
        let name = read_cstr(&data[shstr_off + name_off..]);
        if name != ".text" {
            continue;
        }
        let sec_off = u64::from_le_bytes(data[off + 24..off + 32].try_into().ok()?) as usize;
        let sec_size = u64::from_le_bytes(data[off + 32..off + 40].try_into().ok()?) as usize;
        if sec_off + sec_size > data.len() {
            return None;
        }
        return Some(&data[sec_off..sec_off + sec_size]);
    }
    None
}

// ---- fixtures ----
const FIXTURES: &[&str] = &[
    "struct.c",
    "float.c",
    "builtin_lowlevel.c",
    "string_const.c",
    "stmt_expr.c",
];

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn sha256_path(name: &str) -> std::path::PathBuf {
    let stem = name.trim_end_matches(".c");
    fixtures_dir().join(format!("g10_{}.sha256", stem))
}

fn compile_to_o(name: &str) -> Vec<u8> {
    let src = fixtures_dir().join("c").join(name);
    let dir = std::env::temp_dir();
    let obj = dir.join(format!("dcc-g10-{}-{}.o", name, std::process::id()));
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj)
        .arg(&src)
        .output()
        .expect("run dcc");
    assert!(
        status.status.success(),
        "dcc -c failed for {name}: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let bytes = std::fs::read(&obj).expect("read .o");
    let _ = std::fs::remove_file(&obj);
    bytes
}

#[test]
fn g10_snapshot_text_sha256() {
    let mut generated = Vec::new();
    for name in FIXTURES {
        let obj = compile_to_o(name);
        let text = elf64_text(&obj).expect("missing .text section");
        let hash = sha256_hex(text);
        let path = sha256_path(name);
        match std::fs::read_to_string(&path) {
            Ok(expected) => {
                let expected = expected.trim();
                assert_eq!(
                    expected, hash,
                    "G10 baseline drift for {name}: expected {} got {}",
                    expected, hash
                );
            },
            Err(_) => {
                std::fs::write(&path, format!("{hash}\n")).expect("write baseline");
                generated.push(format!("{name} -> {path:?} ({hash})"));
            },
        }
    }
    if !generated.is_empty() {
        eprintln!("baseline generated:\n  {}", generated.join("\n  "));
    }
}
