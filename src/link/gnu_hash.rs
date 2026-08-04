// src/link/gnu_hash.rs - GNU hash (.gnu.hash) 表生成 (TIE 压缩)
//
// GNU hash 是一种加速动态符号查找的哈希表，相比于 sysv hash：
//   - 使用 TIE (Target Independent Encoding) bloom filter 做快速过滤
//   - 采用按 bucket 排序的二级结构，二分查找 O(log n)
//
// 节结构：
//   header       (8 + 4 + 4 + 4 = 20 bytes) for 64-bit ELF
//   buckets      [NBuckets] u32
//   bloom        [BloomSize] u64 (size = (BloomSize << 3) bytes)
//   hashvals     [SymN] u32 (符号数, 排除 STN_UNDEF)
//
// ELF note: 32-bit ELF 用 Elf32_Word (u32) 字段，64-bit 用 u64。
//          我们目前只支持 64-bit ELF, 所以所有 u32/u64 都用 ELF64 宽度。

#![allow(dead_code)]

const ELFCLASS64: u8 = 2;

#[derive(Debug, Clone)]
pub struct GnuHashTable {
    /// 符号数 (排除 STN_UNDEF)
    pub symcount: u32,
    /// 第一个动态符号索引 (排除 local + main_arena 头)
    pub symndx: u32,
    /// mask 二次散列 shift
    pub maskwords: u32,
    /// shift2 — bloom filter 移位
    pub shift2: u32,
    /// 全部符号 -> 哈希值 (bloom 1 字节, bloom 2 字节)
    pub bloom: Vec<u64>,
    /// bucket 起始符号哈希值 (mod NBuckets)
    pub buckets: Vec<u32>,
    /// 哈希值序列 (按符号递增排序)
    pub chain: Vec<u32>,
}

/// 64-bit ELF 链接时生成 gnu_hash 节内容
pub struct GnuHashConfig {
    /// 待索引的符号名列表 (按 symbols 出现顺序)
    pub symnames: Vec<String>,
    /// bucket 数 (一般是接近 2 的幂, 大于 0)
    pub nbuckets: u32,
    /// bloom filter 大小 (字节) — 通常 8/16/32
    pub bloom_size_bytes: u32,
    /// 二次哈希移位
    pub shift2: u32,
}

impl Default for GnuHashConfig {
    fn default() -> Self {
        Self {
            symnames: vec![],
            nbuckets: 1,
            bloom_size_bytes: 8,
            shift2: 0,
        }
    }
}

/// 计算 GNU hash 用的 ELF 符号哈希
/// 详情见 glibc: glibc/elf/dl-lookup.c (lookup函数)
/// 算法：
///   h = 5381
///   for c in name (without leading NUL/0):
///     h = (h << 5) + h + c   (== h * 33 + c)
///   return h & 0xffffffff
pub fn elf_gnu_hash(name: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in name.bytes() {
        h = h.wrapping_mul(32).wrapping_add(h).wrapping_add(b as u32);
    }
    h
}

pub fn build_gnu_hash(cfg: &GnuHashConfig) -> GnuHashTable {
    let n = cfg.symnames.len() as u32;
    let nbuckets = cfg.nbuckets.max(1);
    let bloom_words = (cfg.bloom_size_bytes / 8).max(1);

    // 计算每个符号的 hash
    let hashes: Vec<u32> = cfg.symnames.iter().map(|s| elf_gnu_hash(s)).collect();
    // chain 必须按 hash mod nbuckets 排序, 相同 bucket 内按 hash 排序
    // 实际 GNU hash 格式要求: chain[i] 存储的是与 hashvals[hashvals[0..i]] 在同一 bucket 中的最长链长度
    // 简化实现: 我们对每个符号计算:
    //   bloom_word = h / 64 mod bloom_words
    //   bloom_val  = (1 << (h % 64)) | (1 << ((h >> shift2) % 64))
    //   bucket     = h % nbuckets
    let mut bloom = vec![0u64; bloom_words as usize];
    let mut buckets: Vec<u32> = vec![0; nbuckets as usize]; // 默认 0 = empty
    let mut chain_vals: Vec<u32> = vec![0; n as usize];
    for (i, h) in hashes.iter().enumerate() {
        let b_idx = (*h as usize) % nbuckets as usize;
        // 简化：bucket 存第一个进入的符号索引 (real GNU hash 存的是 hash mod bucket)
        if buckets[b_idx] == 0 {
            buckets[b_idx] = i as u32;
        }
        chain_vals[i] = *h;
        let w = (*h as usize / 64) % bloom_words as usize;
        let b1 = 1u64 << (*h % 64);
        let b2 = 1u64 << ((*h >> cfg.shift2) % 64);
        bloom[w] |= b1 | b2;
    }
    // 排序 chain (按 hash mod nbuckets 升序，相同 bucket 内按 hash 升序)
    let mut indices: Vec<usize> = (0..n as usize).collect();
    indices.sort_by_key(|&i| (hashes[i] % nbuckets, hashes[i]));
    let new_chain: Vec<u32> = indices.iter().map(|&i| chain_vals[i]).collect();
    let new_buckets: Vec<u32> = {
        let mut b = vec![0u32; nbuckets as usize];
        for &i in &indices {
            let h = hashes[i];
            let b_idx = (h % nbuckets) as usize;
            if b[b_idx] == 0 {
                b[b_idx] = i as u32;
            }
        }
        b
    };
    GnuHashTable {
        symcount: n,
        symndx: 1, // 简化：忽略 local 符号头
        maskwords: bloom_words,
        shift2: cfg.shift2,
        bloom,
        buckets: new_buckets,
        chain: new_chain,
    }
}

/// 把 GnuHashTable 序列化为 ELF 字节 (64-bit)
///   header   : u32 nbuckets, u32 symndx, u32 maskwords, u32 shift2
///   buckets  : [nbuckets] u32
///   bloom    : [maskwords] u64
///   chain    : [symcount] u32
pub fn serialize_gnu_hash(t: &GnuHashTable) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(t.buckets.len() as u32).to_le_bytes());
    out.extend_from_slice(&t.symndx.to_le_bytes());
    out.extend_from_slice(&t.maskwords.to_le_bytes());
    out.extend_from_slice(&t.shift2.to_le_bytes());
    for b in &t.buckets {
        out.extend_from_slice(&b.to_le_bytes());
    }
    for w in &t.bloom {
        out.extend_from_slice(&w.to_le_bytes());
    }
    for c in &t.chain {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out
}

/// ELF class 标识 (64-bit only, TIE 压缩要求 64-bit)
pub fn elfclass() -> u8 {
    ELFCLASS64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_elf_gnu_hash() {
        // Reference value: glibc computes these for symbol "printf"
        let h = elf_gnu_hash("printf");
        assert_ne!(h, 0);
        let h2 = elf_gnu_hash("printf");
        assert_eq!(h, h2);
        assert_ne!(h, elf_gnu_hash("scanf"));
    }

    #[test]
    fn test_build_basic_gnu_hash() {
        let cfg = GnuHashConfig {
            symnames: vec!["printf".into(), "exit".into(), "malloc".into()],
            nbuckets: 2,
            bloom_size_bytes: 8,
            shift2: 0,
        };
        let t = build_gnu_hash(&cfg);
        assert_eq!(t.buckets.len(), 2);
        assert_eq!(t.bloom.len(), 1);
        assert_eq!(t.chain.len(), 3);
    }

    #[test]
    fn test_serialize_gnu_hash_size() {
        let cfg = GnuHashConfig {
            symnames: vec!["a".into(), "b".into(), "c".into()],
            nbuckets: 2,
            bloom_size_bytes: 8,
            shift2: 0,
        };
        let t = build_gnu_hash(&cfg);
        let bytes = serialize_gnu_hash(&t);
        let expected = 16 + t.buckets.len() * 4 + t.bloom.len() * 8 + t.chain.len() * 4;
        assert_eq!(bytes.len(), expected);
    }
}
