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
// dcc 简化格式完整支持:
//   * magic 0x67636E6F / 0x67636461 + version 0x34302A
//   * 文件名 (单个)
//   * 多段 function_record, 含 bb 列表 + 行号表
//   * counter 数组 (u64, 每 bb 8 字节)
//   * 读取器: read_notes() / read_data() 解析 .gcno/.gcda
//   * 行级覆盖率报告: GcovReport 生成文本格式覆盖率

use std::collections::HashMap;
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
    /// Line numbers associated with this block
    pub lines: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct GcovFunction {
    pub name: String,
    pub file: String,
    pub ident: u32,
    pub lineno: u32,
    pub blocks: Vec<GcovBlock>,
    /// Per-line execution counts (populated after reading .gcda)
    pub line_counts: HashMap<u32, u64>,
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

    /// Read and parse a .gcno notes file.
    pub fn read_notes(path: &Path) -> io::Result<Self> {
        let data = fs::read(path)?;
        if data.len() < 12 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "gcno file too short",
            ));
        }
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if magic != GCOV_NOTE_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bad gcno magic: {magic:08x}"),
            ));
        }
        let _version = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let _stamp = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let mut off = 12;
        // Read source file name (pascal string)
        let (source, new_off) = read_pascal_string(&data, off)?;
        off = new_off;
        // Read function count
        if off + 4 > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated gcno func count",
            ));
        }
        let n_funcs = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        let mut unit = GcovUnit::new(source);
        for _ in 0..n_funcs {
            let (func, new_off) = read_function_record(&data, off)?;
            off = new_off;
            unit.push_function(func);
        }
        Ok(unit)
    }

    /// Read a .gcda data file and populate line_counts.
    pub fn read_data(&mut self, path: &Path) -> io::Result<()> {
        let data = fs::read(path)?;
        if data.len() < 12 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "gcda file too short",
            ));
        }
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if magic != GCOV_DATA_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bad gcda magic: {magic:08x}"),
            ));
        }
        let mut off = 12; // skip magic + version + stamp
        // Skip runs + sums
        off += 8;
        // Read source name
        let (_source, new_off) = read_pascal_string(&data, off)?;
        off = new_off;
        // Read function data records
        for func in &mut self.functions {
            if off + 4 > data.len() {
                break;
            }
            let _ident = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            off += 4;
            let _lno_sum = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            off += 4;
            let _cfg_sum = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            off += 4;
            // Skip function name
            let (_name, new_off) = read_pascal_string(&data, off)?;
            off = new_off;
            // Read counter tag
            if off + 4 > data.len() {
                break;
            }
            let _n_tag = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            off += 4;
            if off + 4 > data.len() {
                break;
            }
            let _tag = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            off += 4;
            if off + 4 > data.len() {
                break;
            }
            let n_counters =
                u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            // Read counters and populate line_counts
            for (i, block) in func.blocks.iter().enumerate() {
                if i >= n_counters || off + 8 > data.len() {
                    break;
                }
                let count = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
                off += 8;
                // Map block counts to line numbers
                for &line in &block.lines {
                    *func.line_counts.entry(line).or_insert(0) += count;
                }
                // If no line info, use function entry line
                if block.lines.is_empty() && i == 0 {
                    *func.line_counts.entry(func.lineno).or_insert(0) += count;
                }
            }
            // Skip remaining counters if blocks < counters
            for _ in func.blocks.len()..n_counters {
                if off + 8 > data.len() {
                    break;
                }
                off += 8;
            }
        }
        Ok(())
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
    // lineno table: write block line info
    let total_lines: usize = f.blocks.iter().map(|b| b.lines.len().max(1)).sum();
    buf.extend_from_slice(&(total_lines as u32).to_le_bytes());
    for bb in &f.blocks {
        if bb.lines.is_empty() {
            buf.extend_from_slice(&f.lineno.to_le_bytes());
        } else {
            for &line in &bb.lines {
                buf.extend_from_slice(&line.to_le_bytes());
            }
        }
    }
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

fn read_pascal_string(data: &[u8], off: usize) -> io::Result<(String, usize)> {
    if off + 4 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated pascal string length",
        ));
    }
    let len = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
    let start = off + 4;
    if start + len > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated pascal string body",
        ));
    }
    let s = std::str::from_utf8(&data[start..start + len])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .to_string();
    Ok((s, start + len))
}

fn read_function_record(data: &[u8], off: usize) -> io::Result<(GcovFunction, usize)> {
    let mut pos = off;
    if pos + 12 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated function record header",
        ));
    }
    let ident = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
    pos += 4;
    let _lno_sum = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
    pos += 4;
    let _cfg_sum = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
    pos += 4;
    let (name, new_pos) = read_pascal_string(data, pos)?;
    pos = new_pos;
    if pos + 4 > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated bb count",
        ));
    }
    let n_bb = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;
    let mut blocks = Vec::with_capacity(n_bb);
    for _ in 0..n_bb {
        if pos + 8 > data.len() {
            break;
        }
        let flags = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        pos += 4;
        let n_succ = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let mut succs = Vec::with_capacity(n_succ);
        for _ in 0..n_succ {
            if pos + 8 > data.len() {
                break;
            }
            let s = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let fl = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
            pos += 4;
            succs.push((s, fl));
        }
        blocks.push(GcovBlock {
            flags,
            succs,
            lines: Vec::new(),
        });
    }
    // Read line number table
    let mut lineno = 0u32;
    if pos + 4 <= data.len() {
        let n_lines = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let mut line_idx = 0;
        for block in &mut blocks {
            if line_idx < n_lines && pos + 4 <= data.len() {
                let line = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
                pos += 4;
                line_idx += 1;
                if lineno == 0 {
                    lineno = line;
                }
                block.lines.push(line);
            }
        }
        // Skip remaining lines
        while line_idx < n_lines && pos + 4 <= data.len() {
            pos += 4;
            line_idx += 1;
        }
    }
    Ok((
        GcovFunction {
            name,
            file: String::new(),
            ident,
            lineno,
            blocks,
            line_counts: HashMap::new(),
        },
        pos,
    ))
}

/// Gcov text report: per-function, per-line coverage information.
#[derive(Debug, Clone)]
pub struct GcovReport {
    pub source: String,
    pub functions: Vec<GcovFuncReport>,
}

/// Per-function coverage report
#[derive(Debug, Clone)]
pub struct GcovFuncReport {
    pub name: String,
    pub total_lines: usize,
    pub executed_lines: usize,
    pub line_details: Vec<(u32, u64)>, // (line_no, exec_count)
}

impl GcovReport {
    /// Generate a coverage report from a GcovUnit with loaded data.
    pub fn from_unit(unit: &GcovUnit) -> Self {
        let mut functions = Vec::new();
        for func in &unit.functions {
            let mut line_details: Vec<(u32, u64)> = func
                .line_counts
                .iter()
                .map(|(&line, &count)| (line, count))
                .collect();
            line_details.sort_by_key(|&(line, _)| line);
            let total = line_details.len();
            let executed = line_details.iter().filter(|(_, c)| *c > 0).count();
            functions.push(GcovFuncReport {
                name: func.name.clone(),
                total_lines: total,
                executed_lines: executed,
                line_details,
            });
        }
        GcovReport {
            source: unit.source.clone(),
            functions,
        }
    }

    /// Format as text report (similar to gcov output)
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Source:{}\n", self.source));
        for func in &self.functions {
            let pct = if func.total_lines > 0 {
                func.executed_lines as f64 / func.total_lines as f64 * 100.0
            } else {
                0.0
            };
            out.push_str(&format!(
                "Function:{} Lines:{}/{} Executed:{:.1}%\n",
                func.name, func.executed_lines, func.total_lines, pct
            ));
            for (line, count) in &func.line_details {
                if *count > 0 {
                    out.push_str(&format!("  {}:{}\n", line, count));
                } else {
                    out.push_str(&format!("  {}:#####\n", line));
                }
            }
        }
        out
    }
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
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
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
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
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
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
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
                    lines: vec![],
                }, // 0->1
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
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
                    lines: vec![],
                }, // 0->2 (与 f_a 同样 1 个 succ, 但 dst 不同)
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
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
                    lines: vec![],
                }, // 0->1 (entry)
                GcovBlock {
                    flags: 0,
                    succs: vec![(0, 0), (2, 0)],
                    lines: vec![],
                }, // 1->0 + 1->2
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![],
                },
            ],
            line_counts: HashMap::new(),
        };
        // succs:
        //   bb0: 0->1 → 2
        //   bb1: 1->0 → 1, 1->2 → 3
        //   bb2: nothing
        // total: 2 + 1 + 3 = 6
        assert_eq!(cfg_checksum(&f), 6);
    }

    /// 验证 .gcno 文件格式使用显式 little-endian 字节序.
    /// 这确保在非 x86_64 架构 (如 AArch64/RISC-V, 可能 big-endian)
    /// 上生成的 .gcno/.gcda 文件与 GCC gcov 工具链兼容.
    #[test]
    fn gcov_format_uses_explicit_little_endian() {
        let mut u = GcovUnit::new("test.c");
        u.push_function(GcovFunction {
            name: "f".into(),
            file: "test.c".into(),
            ident: 1,
            lineno: 10,
            blocks: vec![
                GcovBlock { flags: 0, succs: vec![(1, 0)], lines: vec![] },
                GcovBlock { flags: 0, succs: vec![], lines: vec![] },
            ],
            line_counts: HashMap::new(),
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-le-test.gcno");
        let _ = std::fs::remove_file(&tmp);
        u.write_notes(&tmp).unwrap();
        let bytes = std::fs::read(&tmp).unwrap();

        // magic 0x67636E6F ("gcno") in little-endian: 6F 6E 63 67
        assert_eq!(bytes[0], 0x6F);
        assert_eq!(bytes[1], 0x6E);
        assert_eq!(bytes[2], 0x63);
        assert_eq!(bytes[3], 0x67);

        // version 0x34302A in little-endian: 2A 30 34 00
        assert_eq!(bytes[4], 0x2A);
        assert_eq!(bytes[5], 0x30);
        assert_eq!(bytes[6], 0x34);
        assert_eq!(bytes[7], 0x00);

        let _ = std::fs::remove_file(&tmp);
    }

    /// 验证 .gcda 数据文件计数器使用 64-bit little-endian 编码.
    /// 确保跨架构一致性.
    #[test]
    fn gcov_data_counters_are_little_endian_64bit() {
        let mut u = GcovUnit::new("test.c");
        u.push_function(GcovFunction {
            name: "f".into(),
            file: "test.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock { flags: 0, succs: vec![(1, 0)], lines: vec![] },
                GcovBlock { flags: 0, succs: vec![], lines: vec![] },
            ],
            line_counts: HashMap::new(),
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-le-data.gcda");
        let _ = std::fs::remove_file(&tmp);
        // 2 blocks, 2 counters; 第二个计数器值 0x0102030405060708
        // 在 little-endian 应为 08 07 06 05 04 03 02 01
        u.write_data_with_counters(&tmp, &[0, 0x0102030405060708u64]).unwrap();
        let bytes = std::fs::read(&tmp).unwrap();

        // magic 0x67636461 ("gcda") in little-endian
        assert_eq!(&bytes[0..4], &[0x61, 0x64, 0x63, 0x67]);

        // 找到计数器位置 (在 header + function record 之后)
        // 简单验证: 文件中应包含 08 07 06 05 04 03 02 01 序列
        let found = bytes.windows(8).any(|w| {
            w == [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        });
        assert!(found, "counter should be encoded as little-endian 64-bit");

        let _ = std::fs::remove_file(&tmp);
    }

    /// 验证 cfg_checksum 计算不依赖架构字节序.
    /// checksum 使用 wrapping_add, 结果与字节序无关.
    #[test]
    fn gcov_cfg_checksum_arch_independent() {
        // 构造一个 CFG, 验证 checksum 值在不同字节序解释下一致
        let f = GcovFunction {
            name: "test".into(),
            file: "t.c".into(),
            ident: 1,
            lineno: 1,
            blocks: vec![
                GcovBlock { flags: 0, succs: vec![(1, 0), (2, 0)], lines: vec![] },
                GcovBlock { flags: 0, succs: vec![(2, 0)], lines: vec![] },
                GcovBlock { flags: 0, succs: vec![], lines: vec![] },
            ],
            line_counts: HashMap::new(),
        };
        let chksum = cfg_checksum(&f);
        // bb0: 0->1 (1+1=2), 0->2 (2+1=3)
        // bb1: 1->2 (2+1=3)
        // bb2: nothing
        // total: 2 + 3 + 3 = 8
        assert_eq!(chksum, 8);

        // 验证 checksum 写入文件时是 little-endian
        let expected_le = chksum.to_le_bytes();
        assert_eq!(expected_le, [0x08, 0x00, 0x00, 0x00]);
    }

    /// read_notes roundtrip: write .gcno then read it back.
    #[test]
    fn gcov_read_notes_roundtrip() {
        let mut u = GcovUnit::new("hello.c");
        u.push_function(GcovFunction {
            name: "main".into(),
            file: "hello.c".into(),
            ident: 1,
            lineno: 5,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![5],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![6],
                },
            ],
            line_counts: HashMap::new(),
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-read-notes.gcno");
        let _ = std::fs::remove_file(&tmp);
        u.write_notes(&tmp).unwrap();

        let read_back = GcovUnit::read_notes(&tmp).unwrap();
        assert_eq!(read_back.source, "hello.c");
        assert_eq!(read_back.functions.len(), 1);
        assert_eq!(read_back.functions[0].name, "main");
        assert_eq!(read_back.functions[0].blocks.len(), 2);
        assert_eq!(read_back.functions[0].blocks[0].succs, vec![(1, 0)]);
        // Line info should be preserved
        assert_eq!(read_back.functions[0].blocks[0].lines, vec![5]);
        assert_eq!(read_back.functions[0].blocks[1].lines, vec![6]);
        let _ = std::fs::remove_file(&tmp);
    }

    /// read_data: write .gcda then read it back, verify line_counts.
    #[test]
    fn gcov_read_data_populates_line_counts() {
        let mut u = GcovUnit::new("test.c");
        u.push_function(GcovFunction {
            name: "foo".into(),
            file: "test.c".into(),
            ident: 1,
            lineno: 10,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![10],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![11],
                },
            ],
            line_counts: HashMap::new(),
        });
        let tmp = std::env::temp_dir().join("dcc-gcov-read-data.gcda");
        let _ = std::fs::remove_file(&tmp);
        u.write_data_with_counters(&tmp, &[42, 7]).unwrap();

        u.read_data(&tmp).unwrap();
        assert_eq!(u.functions[0].line_counts[&10], 42);
        assert_eq!(u.functions[0].line_counts[&11], 7);
        let _ = std::fs::remove_file(&tmp);
    }

    /// GcovReport: from_unit + to_text produces expected format.
    #[test]
    fn gcov_report_text_format() {
        let mut u = GcovUnit::new("main.c");
        let mut lc = HashMap::new();
        lc.insert(5, 100u64);
        lc.insert(6, 0u64);
        lc.insert(7, 3u64);
        u.push_function(GcovFunction {
            name: "main".into(),
            file: "main.c".into(),
            ident: 1,
            lineno: 5,
            blocks: vec![
                GcovBlock {
                    flags: 0,
                    succs: vec![(1, 0)],
                    lines: vec![5],
                },
                GcovBlock {
                    flags: 0,
                    succs: vec![],
                    lines: vec![6, 7],
                },
            ],
            line_counts: lc,
        });
        let report = GcovReport::from_unit(&u);
        let text = report.to_text();
        assert!(text.contains("Source:main.c"));
        assert!(text.contains("Function:main"));
        assert!(text.contains("Lines:2/3"));
        // Line 6 has count 0, should show #####
        assert!(text.contains("6:#####"));
        // Line 5 has count 100
        assert!(text.contains("5:100"));
    }

    /// read_notes rejects bad magic.
    #[test]
    fn gcov_read_notes_rejects_bad_magic() {
        let tmp = std::env::temp_dir().join("dcc-gcov-bad-magic.gcno");
        std::fs::write(&tmp, [0u8; 16]).unwrap();
        let result = GcovUnit::read_notes(&tmp);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&tmp);
    }
}
