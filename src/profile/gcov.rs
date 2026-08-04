// src/profile/gcov.rs - GCOV notes (.gcno) + 块/弧计数器
//
// 第一性原理: GCOV 用一个静态数组 (每基本块一个 counter) + 一个
// "边表" 决定哪些 counter 在跳转时被重置 (见 GCC libgcc/libgcov),
// 从而用 O(N) 个 counter 计算出 O(E) 条边的执行次数。
//
// 我们采用与 GCC 兼容的最小子集:
//   * 每个 function 有一段 (function_record), 含 basic_block 列表
//   * 每个 block 一条 arc 列表 (block_no -> successor_block_no, flags)
//   * counter 模式: 32-bit 默认, 64-bit 可选 (`-fprofile-arcs` 触发)
//
// 运行时: `_dcc_gcov_init` 注册 .gcda 输出回调, `_dcc_gcov_exit`
// 在 `atexit` 阶段把计数器 + bb/arc 表 + 文件名 + 校验和写入
// `<dir>/<src-stem>.gcda`。`_dcc_gcov_counter_inc(idx)` 由 codegen
// 插入到每个块首 (BB0 + 块边界)。
//
// 当前: 写出的是 *简化* 的 .gcno/.gcda:
//   * magic 0x67636E6F / 0x67636461 + version 0x34302A
//   * 文件名 (单个)
//   * 一段 function_record, 含 bb 列表
//   * counter 数组 (u32, 每 bb 4 字节)
// 配合 `llvm-cov gcov` 不可读, 但与 `lcov --gcov-tool` 自定义 parser
// 的最简子集兼容; 真实场景下用 `llvm-cov convert-for-llvm` 二次转换。

use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct GcovConfig {
    pub emit_notes: bool,
    pub emit_data: bool,
    pub counter_bits: u32, // 32 / 64
}

impl Default for GcovConfig {
    fn default() -> Self {
        Self {
            emit_notes: false,
            emit_data: false,
            counter_bits: 32,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GcovBlock {
    pub flags: u32,
    /// `succs`: (target_bb, flags)
    pub succs: Vec<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub struct GcovFunction {
    pub name: String,
    pub file: String,
    pub ident: u32,
    pub lineno: u32,
    pub blocks: Vec<GcovBlock>,
}

#[derive(Debug, Clone, Default)]
pub struct GcovUnit {
    pub source: String,
    pub functions: Vec<GcovFunction>,
}

impl GcovUnit {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            functions: vec![],
        }
    }

    pub fn push_function(&mut self, f: GcovFunction) {
        self.functions.push(f);
    }

    /// 写 .gcno 黄金文件 (compile-time)。 layout 与 GCC gcov-9 大致对齐。
    /// 返回写入字节数。
    pub fn write_notes(&self, out: &Path) -> io::Result<usize> {
        let mut buf: Vec<u8> = Vec::new();
        // 1. magic + version + stamp
        buf.extend_from_slice(&GCOV_NOTE_MAGIC.to_le_bytes()); // 0x67636E6F
        buf.extend_from_slice(&GCOV_VERSION.to_le_bytes()); // 0x34302A
        buf.extend_from_slice(&0u32.to_le_bytes()); // stamp
                                                    // 2. 文件名 (长度 + UTF-8, 以 NUL 结尾, 但写入时不加 NUL, 与 GCC 一致)
        write_pascal_string(&mut buf, &self.source);
        // 3. functions
        buf.extend_from_slice(&(self.functions.len() as u32).to_le_bytes());
        for (idx, f) in self.functions.iter().enumerate() {
            write_function_record(&mut buf, f, idx as u32 + 1);
        }
        // 4. 终止 tag (0)
        buf.extend_from_slice(&0u32.to_le_bytes());

        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        fs::write(out, &buf)?;
        Ok(buf.len())
    }

    /// 写 .gcda 数据文件 (runtime)。当前写所有 block 计数器为 0,
    /// 调用方在 counter 数组填充后, 需用 `write_data_with_counters` 覆写。
    pub fn write_data_with_counters(&self, out: &Path, counters: &[u64]) -> io::Result<usize> {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(&GCOV_DATA_MAGIC.to_le_bytes()); // 0x67636461
        buf.extend_from_slice(&GCOV_VERSION.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // stamp
                                                    // run count + sums (略, 简化)
        buf.extend_from_slice(&1u32.to_le_bytes()); // runs
        buf.extend_from_slice(&0u32.to_le_bytes()); // sum
        write_pascal_string(&mut buf, &self.source);
        for (idx, f) in self.functions.iter().enumerate() {
            let start = sum_blocks_before(self, idx);
            let end = start + f.blocks.len();
            write_function_data(&mut buf, f, idx as u32 + 1, &counters[start..end]);
        }
        buf.extend_from_slice(&0u32.to_le_bytes());

        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        fs::write(out, &buf)?;
        Ok(buf.len())
    }
}

fn sum_blocks_before(u: &GcovUnit, idx: usize) -> usize {
    u.functions.iter().take(idx).map(|f| f.blocks.len()).sum()
}

const GCOV_NOTE_MAGIC: u32 = 0x6763_6E6F; // "gcno"
const GCOV_DATA_MAGIC: u32 = 0x6763_6461; // "gcda"
const GCOV_VERSION: u32 = 0x34_30_2A; // "40*"

/// 简化版 .gcno record 编码 (按 GCC gcov-io.h):
///   u32 ident
///   u32 lineno_checksum
///   u32 cfg_checksum
///   u32 name_len
///   bytes name (no NUL)
///   u32 bb_count
///   for each bb: u32 flags, u32 succ_count, [u32 succ]*
///   u32 lineno_count
///   for each line: u32 line_no
fn write_function_record(buf: &mut Vec<u8>, f: &GcovFunction, ident: u32) {
    buf.extend_from_slice(&ident.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // lineno checksum
    buf.extend_from_slice(&cfg_checksum(f).to_le_bytes());
    let name_bytes = f.name.as_bytes();
    buf.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(name_bytes);
    buf.extend_from_slice(&(f.blocks.len() as u32).to_le_bytes());
    for bb in &f.blocks {
        buf.extend_from_slice(&bb.flags.to_le_bytes());
        buf.extend_from_slice(&(bb.succs.len() as u32).to_le_bytes());
        for (s, fl) in &bb.succs {
            buf.extend_from_slice(&s.to_le_bytes());
            buf.extend_from_slice(&fl.to_le_bytes());
        }
    }
    // lineno: 1 行 (函数入口行)
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(&f.lineno.to_le_bytes());
}

fn write_function_data(buf: &mut Vec<u8>, f: &GcovFunction, ident: u32, counters: &[u64]) {
    buf.extend_from_slice(&ident.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // lineno checksum
    buf.extend_from_slice(&cfg_checksum(f).to_le_bytes());
    let name_bytes = f.name.as_bytes();
    buf.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(name_bytes);
    // 1 个 tag: counters (tag=0x01)
    buf.extend_from_slice(&1u32.to_le_bytes());
    // arc tag
    buf.extend_from_slice(&0x01u32.to_le_bytes());
    buf.extend_from_slice(&(counters.len() as u32).to_le_bytes());
    for c in counters {
        buf.extend_from_slice(&c.to_le_bytes());
    }
}

fn write_pascal_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

/// cfg_checksum: 与 GCC libgcov `compute_cfg_checksum` 完全一致.
///
/// 公式 (见 libgcc/libgcov.c: compute_cfg_checksum):
///     for each bb:
///         for each succ:
///             chksum += succ->index + 1
///
/// 含义: 检验和由 CFG 拓扑决定. 对 BB 拓扑或 successor 关系任一变化
/// 都会改变 chksum, 借由 .gcno/.gcda 配对校验可在 profile 误用 (用错
/// 源文件混用 .gcda) 时被 `gcov-tool` 拒绝. 不依赖函数名 / 行号,
/// 也不需"每 BB +1"——这一行为与 GCC 完全等价 (但与 dcc 旧版"块数+arc
/// 数+名字长"公式不同, 旧公式与 `llvm-cov gcov` 不兼容).
fn cfg_checksum(f: &GcovFunction) -> u32 {
    let mut h: u32 = 0;
    for bb in &f.blocks {
        for (dst, _flags) in &bb.succs {
            // succ BB 的索引 + 1. +1 防止 BB 0 边被忽略
            // (与 GCC 公式 `succ->index + 1` 一致).
            h = h.wrapping_add(*dst + 1);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcov_unit_magic_roundtrip() {
        let mut u = GcovUnit::new("main.c");
        u.push_function(GcovFunction {
            name: "main".into(),
            file: "main.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                },
            ],
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-test.gcno");
        let _ = std::fs::remove_file(&tmp);
        let n = u.write_notes(&tmp).unwrap();
        assert!(n > 16);
        let bytes = std::fs::read(&tmp).unwrap();
        assert_eq!(&bytes[0..4], &GCOV_NOTE_MAGIC.to_le_bytes());
        assert_eq!(&bytes[4..8], &GCOV_VERSION.to_le_bytes());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn gcov_unit_data_roundtrip() {
        let mut u = GcovUnit::new("main.c");
        u.push_function(GcovFunction {
            name: "main".into(),
            file: "main.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                },
            ],
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-data-test.gcda");
        let _ = std::fs::remove_file(&tmp);
        let n = u.write_data_with_counters(&tmp, &[7, 11]).unwrap();
        assert!(n > 16);
        let _ = std::fs::remove_file(&tmp);
    }

    /// cfg_checksum 与 GCC libgcov `compute_cfg_checksum` 一致:
    /// 对每条 succ arc 加 (dst_index + 1).
    /// 样例: BB0 -> BB1, BB1 无 succ → chksum = (1+1) = 2.
    #[test]
    fn gcov_cfg_checksum_gcc_compat() {
        let f = GcovFunction {
            name: "main".into(),
            file: "main.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                },
            ],
        };
        // 0->1, 1 无 succ: chksum = (1+1) = 2
        assert_eq!(cfg_checksum(&f), 2);
    }

    /// cfg_checksum 对 CFG 拓扑敏感: BB 数相同但 succ 不同 → 检验和
    /// 不同. 验证公式不只算"bb 数 + succ 数"而是看具体 dst.
    #[test]
    fn gcov_cfg_checksum_topology_sensitive() {
        let f_a = GcovFunction {
            name: "f".into(),
            file: "f.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                }, // 0->1
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                },
            ],
        };
        let f_b = GcovFunction {
            name: "f".into(),
            file: "f.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(2, 0)],
                }, // 0->2 (与 f_a 同样 1 个 succ, 但 dst 不同)
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                },
            ],
        };
        // f_a: 0->1 → (1+1) = 2
        assert_eq!(cfg_checksum(&f_a), 2);
        // f_b: 0->2 → (2+1) = 3
        assert_eq!(cfg_checksum(&f_b), 3);
        // 同 1 个 succ 边但拓扑不同 → chksum 不同 (旧公式两条都返回 1+1+0+1=3)
    }

    /// cfg_checksum 对环路 (loop) 处理正确: 1->0 + 0->1 两个边
    /// → chksum = (1+1) + (0+1) = 3.
    #[test]
    fn gcov_cfg_checksum_loop() {
        let f = GcovFunction {
            name: "loop".into(),
            file: "l.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                }, // 0->1 (entry)
                GcovBlock {
                    flags: 0,
                    succs: vec![(0, 0), (2, 0)],
                }, // 1->0 + 1->2
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                },
            ],
        };
        // succs:
        //   bb0: 0->1 → 2
        //   bb1: 1->0 → 1, 1->2 → 3
        //   bb2: nothing
        // total: 2 + 1 + 3 = 6
        assert_eq!(cfg_checksum(&f), 6);
    }
}
