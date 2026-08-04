// src/bootstrap.rs - 自举回归对比工具
//
// 第一性原理: dcc0 (rustc 编译产物) 与 dcc1 (dcc0 自举产物) 应能产生
// "等价"的目标文件. 严格的字节级等价很难达成 (路径/时间戳/符号序等噪声),
// 因此提供三层对比:
//
//   1. 字节级 (byte_diff): .text / .rodata 段内容逐字节比较. 用于快速
//      发现编译 pipeline 引入的纯汇编层面错误 (例如 mov→lea 误改).
//   2. 结构级 (struct_diff): 节名+大小+类型集合对比. 用于发现节布局
//      退化 (例如 .text 消失, .data 异常扩大).
//   3. 语义级 (semantic_diff): 全局符号集合+大小对比+引用关系对比.
//      用于发现符号可见性变化、缺失、引用错位.
//
// 调用方: 主入口 `compare(left, right) -> DiffReport`. 可在 driver 端
// 通过 `-fbootstrap-compare=ref.o` 触发, 失败时不阻塞编译 (警告级别),
// 与 GCC `-fcompare-debug` 行为一致.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;

/// 比对结果汇总.
#[derive(Debug, Clone, Default)]
pub struct DiffReport {
    pub byte_diffs: Vec<ByteDiff>,
    pub section_diffs: Vec<String>,
    pub symbol_diffs: Vec<SymbolDiff>,
    pub referenced_diffs: Vec<String>,
    /// 整体是否 "语义等价": 结构级 + 符号级 + 引用级都通过, 即使字节级
    /// 存在差异 (例如 LEA 优化器选不同编码).
    pub semantically_equivalent: bool,
}

#[derive(Debug, Clone)]
pub struct ByteDiff {
    pub section: String,
    pub left_size: usize,
    pub right_size: usize,
    /// 前 N 个不同字节的位置 (限 16, 避免报告过长).
    pub differing_offsets: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct SymbolDiff {
    pub kind: SymbolDiffKind,
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolDiffKind {
    Added,       // 右文件新增
    Removed,     // 右文件缺失
    SizeChanged, // 大小变化
    BindChanged, // 绑定变化
}

/// 解析 .text / .rodata 段载荷, 返回 HashMap<name, bytes> (None 表示无此段).
fn read_sections(elf: &[u8]) -> Option<HashMap<String, Vec<u8>>> {
    if elf.len() < 64
        || elf.get(0..4) != Some(&crate::codegen::elf::ELFMAG)
        || elf[4] != crate::codegen::elf::ELFCLASS64
    {
        return None;
    }
    let shoff = u64::from_le_bytes(elf.get(40..48)?.try_into().ok()?) as usize;
    let shentsize = u16::from_le_bytes(elf.get(58..60)?.try_into().ok()?) as usize;
    let shnum = u16::from_le_bytes(elf.get(60..62)?.try_into().ok()?) as usize;
    let shstrndx = u16::from_le_bytes(elf.get(62..64)?.try_into().ok()?) as usize;
    if shnum == 0 || shentsize < 64 || shstrndx >= shnum {
        return None;
    }
    let shstr_hdr = shoff + shstrndx * shentsize;
    if shstr_hdr + 64 > elf.len() {
        return None;
    }
    let shstr_off =
        u64::from_le_bytes(elf[shstr_hdr + 24..shstr_hdr + 32].try_into().ok()?) as usize;
    let shstr_size =
        u64::from_le_bytes(elf[shstr_hdr + 32..shstr_hdr + 40].try_into().ok()?) as usize;
    if shstr_off + shstr_size > elf.len() {
        return None;
    }
    let shstrtab = &elf[shstr_off..shstr_off + shstr_size];

    let mut out = HashMap::new();
    for i in 0..shnum {
        let base = shoff + i * shentsize;
        if base + 64 > elf.len() {
            break;
        }
        let name_idx = u32::from_le_bytes(elf[base..base + 4].try_into().ok()?) as usize;
        let data_off = u64::from_le_bytes(elf[base + 24..base + 32].try_into().ok()?) as usize;
        let data_size = u64::from_le_bytes(elf[base + 32..base + 40].try_into().ok()?) as usize;
        if data_size == 0 {
            continue;
        }
        if data_off + data_size > elf.len() {
            continue;
        }
        let name = read_cstr(shstrtab, name_idx)?;
        // 只读 PROGBITS / NOBITS 之外还要 .text / .rodata / .data
        if name.starts_with(".text") || name.starts_with(".rodata") || name.starts_with(".data") {
            out.insert(
                name.to_string(),
                elf[data_off..data_off + data_size].to_vec(),
            );
        }
    }
    Some(out)
}

fn read_cstr(data: &[u8], off: usize) -> Option<&str> {
    let end = data.get(off..)?.iter().position(|&b| b == 0)?;
    std::str::from_utf8(&data[off..off + end]).ok()
}

/// 全局符号集合: 解析 .symtab + .strtab, 返回 (name, size, bind) 列表.
/// bind: 1=STB_GLOBAL, 2=STB_WEAK, 其它忽略.
fn read_global_symbols(elf: &[u8]) -> Vec<(String, u64, u8)> {
    let mut out = Vec::new();
    if elf.len() < 64 {
        return out;
    }
    let shoff = match u64::from_le_bytes(
        elf.get(40..48)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 8]),
    ) {
        v if v > 0 => v as usize,
        _ => return out,
    };
    let shentsize = u16::from_le_bytes(
        elf.get(58..60)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    let shnum = u16::from_le_bytes(
        elf.get(60..62)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    let shstrndx = u16::from_le_bytes(
        elf.get(62..64)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    if shnum == 0 || shentsize < 64 || shstrndx >= shnum {
        return out;
    }
    let shstr_hdr = shoff + shstrndx * shentsize;
    if shstr_hdr + 64 > elf.len() {
        return out;
    }
    let shstr_off =
        u64::from_le_bytes(elf[shstr_hdr + 24..shstr_hdr + 32].try_into().unwrap()) as usize;
    let shstr_size =
        u64::from_le_bytes(elf[shstr_hdr + 32..shstr_hdr + 40].try_into().unwrap()) as usize;
    if shstr_off + shstr_size > elf.len() {
        return out;
    }
    let shstrtab = &elf[shstr_off..shstr_off + shstr_size];

    let mut symtab: Option<(usize, usize, u64)> = None;
    let mut strtab: Option<(usize, usize)> = None;
    for i in 0..shnum {
        let base = shoff + i * shentsize;
        if base + 64 > elf.len() {
            break;
        }
        let name_idx = u32::from_le_bytes(elf[base..base + 4].try_into().unwrap()) as usize;
        let sh_type = u32::from_le_bytes(elf[base + 4..base + 8].try_into().unwrap());
        let data_off = u64::from_le_bytes(elf[base + 24..base + 32].try_into().unwrap()) as usize;
        let data_size = u64::from_le_bytes(elf[base + 32..base + 40].try_into().unwrap()) as usize;
        let entsize = u64::from_le_bytes(elf[base + 56..base + 64].try_into().unwrap());
        let name = read_cstr(shstrtab, name_idx).unwrap_or("").to_string();
        if name == ".symtab" && sh_type == 2 {
            symtab = Some((data_off, data_size, entsize));
        } else if name == ".strtab" {
            strtab = Some((data_off, data_size));
        }
    }
    let (sym_off, sym_size, sym_entsize) = match symtab {
        Some(v) if v.2 >= 24 => v,
        _ => return out,
    };
    let (str_off, str_size) = match strtab {
        Some(v) => v,
        _ => return out,
    };
    if sym_off + sym_size > elf.len() || str_off + str_size > elf.len() {
        return out;
    }
    let symtab_data = &elf[sym_off..sym_off + sym_size];
    let strtab_data = &elf[str_off..str_off + str_size];
    let sym_count = sym_size / sym_entsize as usize;

    for i in 0..sym_count {
        let base = i * sym_entsize as usize;
        if base + 24 > symtab_data.len() {
            break;
        }
        let st_name = u32::from_le_bytes(symtab_data[base..base + 4].try_into().unwrap()) as usize;
        let st_info = symtab_data[base + 4];
        let st_size = u64::from_le_bytes(symtab_data[base + 16..base + 24].try_into().unwrap());
        let bind = st_info >> 4;
        if bind != 1 && bind != 2 {
            continue;
        }
        let name = read_cstr(strtab_data, st_name).unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        out.push((name, st_size, bind));
    }
    out
}

/// 段列表 (name, type, size), 用于结构级 diff.
fn read_section_list(elf: &[u8]) -> Vec<(String, u32, u64)> {
    let mut out = Vec::new();
    if elf.len() < 64 {
        return out;
    }
    let shoff = match u64::from_le_bytes(
        elf.get(40..48)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 8]),
    ) {
        v if v > 0 => v as usize,
        _ => return out,
    };
    let shentsize = u16::from_le_bytes(
        elf.get(58..60)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    let shnum = u16::from_le_bytes(
        elf.get(60..62)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    let shstrndx = u16::from_le_bytes(
        elf.get(62..64)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    if shnum == 0 || shentsize < 64 || shstrndx >= shnum {
        return out;
    }
    let shstr_hdr = shoff + shstrndx * shentsize;
    if shstr_hdr + 64 > elf.len() {
        return out;
    }
    let shstr_off =
        u64::from_le_bytes(elf[shstr_hdr + 24..shstr_hdr + 32].try_into().unwrap()) as usize;
    let shstr_size =
        u64::from_le_bytes(elf[shstr_hdr + 32..shstr_hdr + 40].try_into().unwrap()) as usize;
    if shstr_off + shstr_size > elf.len() {
        return out;
    }
    let shstrtab = &elf[shstr_off..shstr_off + shstr_size];
    for i in 0..shnum {
        let base = shoff + i * shentsize;
        if base + 64 > elf.len() {
            break;
        }
        let name_idx = u32::from_le_bytes(elf[base..base + 4].try_into().unwrap()) as usize;
        let sh_type = u32::from_le_bytes(elf[base + 4..base + 8].try_into().unwrap());
        let sh_size = u64::from_le_bytes(elf[base + 32..base + 40].try_into().unwrap());
        let name = read_cstr(shstrtab, name_idx).unwrap_or("").to_string();
        out.push((name, sh_type, sh_size));
    }
    out
}

/// 字节级 diff: 同名段逐字节比较, 限 16 个不同偏移.
fn byte_diff_sections(
    left: &HashMap<String, Vec<u8>>,
    right: &HashMap<String, Vec<u8>>,
) -> Vec<ByteDiff> {
    let names: BTreeSet<&String> = left.keys().chain(right.keys()).collect();
    let mut diffs = Vec::new();
    for name in names {
        let l = left.get(name);
        let r = right.get(name);
        if l.is_none() || r.is_none() {
            continue; // 段级差异已由 struct_diff 捕获
        }
        let lb = l.unwrap();
        let rb = r.unwrap();
        if lb == rb {
            continue;
        }
        let mut offsets = Vec::new();
        let min = lb.len().min(rb.len());
        for i in 0..min {
            if lb[i] != rb[i] {
                if offsets.len() >= 16 {
                    break;
                }
                offsets.push(i as u32);
            }
        }
        // 长度差也算作"不同" (记录到不同 offset 之外, 隐含在 size 不等中)
        diffs.push(ByteDiff {
            section: name.clone(),
            left_size: lb.len(),
            right_size: rb.len(),
            differing_offsets: offsets,
        });
    }
    diffs
}

/// 结构级 diff: 节集合 + 类型 + 体积比较.
fn struct_diff_sections(left: &[(String, u32, u64)], right: &[(String, u32, u64)]) -> Vec<String> {
    let left_map: HashMap<&str, (u32, u64)> = left
        .iter()
        .map(|(n, t, s)| (n.as_str(), (*t, *s)))
        .collect();
    let right_map: HashMap<&str, (u32, u64)> = right
        .iter()
        .map(|(n, t, s)| (n.as_str(), (*t, *s)))
        .collect();
    let all: BTreeSet<&str> = left_map.keys().chain(right_map.keys()).copied().collect();
    let mut diffs = Vec::new();
    for name in all {
        match (left_map.get(name), right_map.get(name)) {
            (Some(l), Some(r)) => {
                if l != r {
                    diffs.push(format!(
                        "section {name}: left type={} size={} right type={} size={}",
                        l.0, l.1, r.0, r.1
                    ));
                }
            }
            (Some(_), None) => {
                diffs.push(format!("section {name}: removed in right"));
            }
            (None, Some(_)) => {
                diffs.push(format!("section {name}: added in right"));
            }
            (None, None) => unreachable!(),
        }
    }
    diffs
}

/// 语义级 diff: 全局符号 + 引用关系比较.
/// 返回 symbol_diffs (add/remove/size/bind) + referenced_diffs (引用集合差).
fn semantic_diff(left_elf: &[u8], right_elf: &[u8]) -> (Vec<SymbolDiff>, Vec<String>) {
    let left_syms = read_global_symbols(left_elf);
    let right_syms = read_global_symbols(right_elf);
    let left_map: HashMap<&str, (u64, u8)> = left_syms
        .iter()
        .map(|(n, s, b)| (n.as_str(), (*s, *b)))
        .collect();
    let right_map: HashMap<&str, (u64, u8)> = right_syms
        .iter()
        .map(|(n, s, b)| (n.as_str(), (*s, *b)))
        .collect();
    let all: BTreeSet<&str> = left_map.keys().chain(right_map.keys()).copied().collect();
    let mut sym_diffs = Vec::new();
    for name in all {
        match (left_map.get(name), right_map.get(name)) {
            (Some(l), Some(r)) => {
                if l.0 != r.0 {
                    sym_diffs.push(SymbolDiff {
                        kind: SymbolDiffKind::SizeChanged,
                        name: name.to_string(),
                        detail: format!("size {} -> {}", l.0, r.0),
                    });
                }
                if l.1 != r.1 {
                    let b = |v: u8| match v {
                        1 => "GLOBAL",
                        2 => "WEAK",
                        _ => "?",
                    };
                    sym_diffs.push(SymbolDiff {
                        kind: SymbolDiffKind::BindChanged,
                        name: name.to_string(),
                        detail: format!("bind {} -> {}", b(l.1), b(r.1)),
                    });
                }
            }
            (Some(l), None) => {
                sym_diffs.push(SymbolDiff {
                    kind: SymbolDiffKind::Removed,
                    name: name.to_string(),
                    detail: format!("size={} bind={}", l.0, l.1),
                });
            }
            (None, Some(r)) => {
                sym_diffs.push(SymbolDiff {
                    kind: SymbolDiffKind::Added,
                    name: name.to_string(),
                    detail: format!("size={} bind={}", r.0, r.1),
                });
            }
            (None, None) => unreachable!(),
        }
    }
    // 引用关系 diff
    let left_refs: BTreeSet<String> = crate::codegen::elf::referenced_symbol_names(left_elf)
        .into_iter()
        .collect();
    let right_refs: BTreeSet<String> = crate::codegen::elf::referenced_symbol_names(right_elf)
        .into_iter()
        .collect();
    let mut ref_diffs = Vec::new();
    for n in left_refs.difference(&right_refs) {
        ref_diffs.push(format!("ref removed: {n}"));
    }
    for n in right_refs.difference(&left_refs) {
        ref_diffs.push(format!("ref added: {n}"));
    }
    (sym_diffs, ref_diffs)
}

/// 主入口: 从文件路径加载并对比.
pub fn compare_files(left: &Path, right: &Path) -> Result<DiffReport, String> {
    let l = fs::read(left).map_err(|e| format!("read {}: {}", left.display(), e))?;
    let r = fs::read(right).map_err(|e| format!("read {}: {}", right.display(), e))?;
    compare(&l, &r)
}

/// 主入口: 直接对内存中的两份 ELF 字节做对比.
pub fn compare(left: &[u8], right: &[u8]) -> Result<DiffReport, String> {
    let l_secs = read_sections(left).ok_or_else(|| "left: not a valid ELF64".to_string())?;
    let r_secs = read_sections(right).ok_or_else(|| "right: not a valid ELF64".to_string())?;
    let byte_diffs = byte_diff_sections(&l_secs, &r_secs);

    let l_sec_list = read_section_list(left);
    let r_sec_list = read_section_list(right);
    let section_diffs = struct_diff_sections(&l_sec_list, &r_sec_list);

    let (symbol_diffs, referenced_diffs) = semantic_diff(left, right);

    // 语义等价 = 节结构完全一致 + 符号集合完全一致 + 引用集合完全一致
    // (允许 byte-level 差异, 例如 LEA/MOV 选择不同)
    let semantically_equivalent =
        section_diffs.is_empty() && symbol_diffs.is_empty() && referenced_diffs.is_empty();

    Ok(DiffReport {
        byte_diffs,
        section_diffs,
        symbol_diffs,
        referenced_diffs,
        semantically_equivalent,
    })
}

impl DiffReport {
    /// 渲染为人类可读多行字符串, 供 stderr 报告用.
    pub fn format(&self) -> String {
        let mut out = String::new();
        if self.byte_diffs.is_empty()
            && self.section_diffs.is_empty()
            && self.symbol_diffs.is_empty()
            && self.referenced_diffs.is_empty()
        {
            out.push_str("bootstrap: byte + semantic identical\n");
            return out;
        }
        if !self.byte_diffs.is_empty() {
            out.push_str(&format!(
                "byte diffs: {} sections differ\n",
                self.byte_diffs.len()
            ));
            for d in &self.byte_diffs {
                out.push_str(&format!(
                    "  .{}: {}B vs {}B, first offsets: {:?}\n",
                    d.section, d.left_size, d.right_size, d.differing_offsets
                ));
            }
        }
        if !self.section_diffs.is_empty() {
            out.push_str(&format!("section diffs: {}\n", self.section_diffs.len()));
            for s in &self.section_diffs {
                out.push_str(&format!("  {s}\n"));
            }
        }
        if !self.symbol_diffs.is_empty() {
            out.push_str(&format!("symbol diffs: {}\n", self.symbol_diffs.len()));
            for s in &self.symbol_diffs {
                out.push_str(&format!("  [{:?}] {} {}\n", s.kind, s.name, s.detail));
            }
        }
        if !self.referenced_diffs.is_empty() {
            out.push_str(&format!("ref diffs: {}\n", self.referenced_diffs.len()));
            for s in &self.referenced_diffs {
                out.push_str(&format!("  {s}\n"));
            }
        }
        out.push_str(&format!(
            "semantically_equivalent: {}\n",
            self.semantically_equivalent
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::elf;

    /// 构造一个最小 ET_REL ELF64 .o, 包含 .text + .data + .symtab + .strtab.
    /// 用于 bootstrap diff 单元测试.
    fn build_minimal_rel(text: &[u8], data: &[u8], syms: &[(&str, u64, u8)]) -> Vec<u8> {
        // 段布局:
        //   ELF header (64B)
        //   .text (align 1)
        //   .data (align 1)
        //   .strtab (align 1)
        //   .symtab (align 8) - 24B / sym
        //   .shstrtab (align 1)
        //   section header table (6 entries * 64B)
        // shstrtab 内容: \0 .text \0 .data \0 .symtab \0 .strtab \0 .shstrtab \0
        let shstrtab = b"\0.text\0.data\0.symtab\0.strtab\0.shstrtab\0";
        const SH_NAME_TEXT: u32 = 1;
        const SH_NAME_DATA: u32 = 7;
        const SH_NAME_SYMTAB: u32 = 13;
        const SH_NAME_STRTAB: u32 = 21;
        const SH_NAME_SHSTRTAB: u32 = 29;

        // 构造 strtab + 记录每个 sym 的 name offset
        let mut strtab2 = vec![0u8]; // 索引 0 = ""
        let mut offsets = vec![0usize; syms.len()];
        for (i, (n, _, _)) in syms.iter().enumerate() {
            offsets[i] = strtab2.len();
            strtab2.extend_from_slice(n.as_bytes());
            strtab2.push(0);
        }

        // 写各段到 out
        let mut out = vec![0u8; 64]; // ELF header 占位
        let text_off = out.len();
        out.extend_from_slice(text);
        let data_off = out.len();
        out.extend_from_slice(data);
        let strtab_off = out.len();
        out.extend_from_slice(&strtab2);
        let symtab_off = out.len();
        // null sym (24B)
        out.resize(out.len() + 24, 0);
        for (i, (_, size, bind)) in syms.iter().enumerate() {
            out.extend_from_slice(&(offsets[i] as u32).to_le_bytes());
            out.push(*bind << 4); // st_info: bind<<4 | STT_NOTYPE
            out.push(0); // st_other
            out.extend_from_slice(&1u16.to_le_bytes()); // st_shndx: 1 (.text)
            out.extend_from_slice(&0u64.to_le_bytes()); // st_value
            out.extend_from_slice(&size.to_le_bytes());
        }
        let shstrtab_off = out.len();
        out.extend_from_slice(shstrtab);

        // 对齐到 8
        while !out.len().is_multiple_of(8) {
            out.push(0);
        }

        // 6 section headers
        let sh_off = out.len();
        out.resize(out.len() + 6 * 64, 0);

        // 填充 ELF header
        out[0..4].copy_from_slice(&elf::ELFMAG);
        out[4] = elf::ELFCLASS64;
        out[5] = elf::ELFDATA2LSB;
        out[6] = elf::EV_CURRENT;
        out[16..18].copy_from_slice(&elf::ET_REL.to_le_bytes());
        out[18..20].copy_from_slice(&elf::EM_X86_64.to_le_bytes());
        out[20..24].copy_from_slice(&1u32.to_le_bytes()); // e_version
        out[24..32].copy_from_slice(&0u64.to_le_bytes()); // e_entry
        out[32..40].copy_from_slice(&0u64.to_le_bytes()); // e_phoff
        out[40..48].copy_from_slice(&(sh_off as u64).to_le_bytes()); // e_shoff
        out[48..52].copy_from_slice(&0u32.to_le_bytes()); // e_flags
        out[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
        out[54..56].copy_from_slice(&0u16.to_le_bytes()); // e_phentsize
        out[56..58].copy_from_slice(&0u16.to_le_bytes()); // e_phnum
        out[58..60].copy_from_slice(&64u16.to_le_bytes()); // e_shentsize
        out[60..62].copy_from_slice(&6u16.to_le_bytes()); // e_shnum
        out[62..64].copy_from_slice(&5u16.to_le_bytes()); // e_shstrndx (5 = .shstrtab)

        // 写 section headers
        // 0: null
        // 1: .text (SHT_PROGBITS, SHF_ALLOC|SHF_EXECINSTR)
        // 2: .data (SHT_PROGBITS, SHF_ALLOC|SHF_WRITE)
        // 3: .symtab (SHT_SYMTAB), link=.strtab idx 4, info=first non-local sym idx
        // 4: .strtab (SHT_STRTAB)
        // 5: .shstrtab (SHT_STRTAB)
        let write_sh = |buf: &mut Vec<u8>,
                        idx: usize,
                        name_off: u32,
                        sh_type: u32,
                        flags: u64,
                        addr: u64,
                        offset: u64,
                        size: u64,
                        link: u32,
                        info: u32,
                        addralign: u64,
                        entsize: u64| {
            let base = sh_off + idx * 64;
            buf[base..base + 4].copy_from_slice(&name_off.to_le_bytes());
            buf[base + 4..base + 8].copy_from_slice(&sh_type.to_le_bytes());
            buf[base + 8..base + 16].copy_from_slice(&flags.to_le_bytes());
            buf[base + 16..base + 24].copy_from_slice(&addr.to_le_bytes());
            buf[base + 24..base + 32].copy_from_slice(&offset.to_le_bytes());
            buf[base + 32..base + 40].copy_from_slice(&size.to_le_bytes());
            buf[base + 40..base + 44].copy_from_slice(&link.to_le_bytes());
            buf[base + 44..base + 48].copy_from_slice(&info.to_le_bytes());
            buf[base + 48..base + 56].copy_from_slice(&addralign.to_le_bytes());
            buf[base + 56..base + 64].copy_from_slice(&entsize.to_le_bytes());
        };
        write_sh(&mut out, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        write_sh(
            &mut out,
            1,
            SH_NAME_TEXT,
            elf::SHT_PROGBITS,
            0x6,
            0,
            text_off as u64,
            text.len() as u64,
            0,
            0,
            1,
            0,
        );
        write_sh(
            &mut out,
            2,
            SH_NAME_DATA,
            elf::SHT_PROGBITS,
            0x3,
            0,
            data_off as u64,
            data.len() as u64,
            0,
            0,
            1,
            0,
        );
        // .symtab: link=4 (.strtab), info=1 (第一个 non-local sym index)
        let first_global = 1u32;
        write_sh(
            &mut out,
            3,
            SH_NAME_SYMTAB,
            elf::SHT_SYMTAB,
            0,
            0,
            symtab_off as u64,
            (24 * (1 + syms.len())) as u64,
            4,
            first_global,
            8,
            24,
        );
        write_sh(
            &mut out,
            4,
            SH_NAME_STRTAB,
            elf::SHT_STRTAB,
            0,
            0,
            strtab_off as u64,
            strtab2.len() as u64,
            0,
            0,
            1,
            0,
        );
        write_sh(
            &mut out,
            5,
            SH_NAME_SHSTRTAB,
            elf::SHT_STRTAB,
            0,
            0,
            shstrtab_off as u64,
            shstrtab.len() as u64,
            0,
            0,
            1,
            0,
        );

        out
    }

    #[test]
    fn identical_files_report_clean() {
        let elf = build_minimal_rel(b"hello", b"world", &[("foo", 5, 1)]);
        let r = compare(&elf, &elf).unwrap();
        assert!(r.byte_diffs.is_empty());
        assert!(r.section_diffs.is_empty());
        assert!(r.symbol_diffs.is_empty());
        assert!(r.semantically_equivalent);
    }

    #[test]
    fn byte_diff_in_text_detected() {
        let l = build_minimal_rel(b"hello", b"world", &[("foo", 5, 1)]);
        let r = build_minimal_rel(b"hellz", b"world", &[("foo", 5, 1)]);
        let rep = compare(&l, &r).unwrap();
        assert_eq!(rep.byte_diffs.len(), 1);
        assert_eq!(rep.byte_diffs[0].section, ".text");
        assert_eq!(rep.byte_diffs[0].differing_offsets, vec![4]);
        // 其它层面等价
        assert!(rep.section_diffs.is_empty());
        assert!(rep.symbol_diffs.is_empty());
        assert!(rep.semantically_equivalent);
    }

    #[test]
    fn symbol_added_detected() {
        let l = build_minimal_rel(b"abc", b"x", &[("foo", 3, 1)]);
        let r = build_minimal_rel(b"abc", b"x", &[("foo", 3, 1), ("bar", 1, 1)]);
        let rep = compare(&l, &r).unwrap();
        assert_eq!(rep.symbol_diffs.len(), 1);
        assert_eq!(rep.symbol_diffs[0].kind, SymbolDiffKind::Added);
        assert_eq!(rep.symbol_diffs[0].name, "bar");
        assert!(!rep.semantically_equivalent);
    }

    #[test]
    fn symbol_removed_detected() {
        let l = build_minimal_rel(b"abc", b"x", &[("foo", 3, 1), ("bar", 1, 1)]);
        let r = build_minimal_rel(b"abc", b"x", &[("foo", 3, 1)]);
        let rep = compare(&l, &r).unwrap();
        assert_eq!(rep.symbol_diffs.len(), 1);
        assert_eq!(rep.symbol_diffs[0].kind, SymbolDiffKind::Removed);
        assert_eq!(rep.symbol_diffs[0].name, "bar");
        assert!(!rep.semantically_equivalent);
    }

    #[test]
    fn symbol_size_changed_detected() {
        let l = build_minimal_rel(b"abc", b"x", &[("foo", 3, 1)]);
        let r = build_minimal_rel(b"abc", b"x", &[("foo", 5, 1)]);
        let rep = compare(&l, &r).unwrap();
        assert_eq!(rep.symbol_diffs.len(), 1);
        assert_eq!(rep.symbol_diffs[0].kind, SymbolDiffKind::SizeChanged);
        assert!(!rep.semantically_equivalent);
    }

    #[test]
    fn section_layout_difference_detected() {
        // .data 大小不同 -> 结构差异
        let l = build_minimal_rel(b"abc", b"xyzw", &[("foo", 3, 1)]);
        let r = build_minimal_rel(b"abc", b"x", &[("foo", 3, 1)]);
        let rep = compare(&l, &r).unwrap();
        // 字节级 + 结构级都会报
        assert!(!rep.byte_diffs.is_empty() || !rep.section_diffs.is_empty());
    }

    #[test]
    fn invalid_elf_returns_error() {
        let r = compare(b"not an elf", b"also not");
        assert!(r.is_err());
    }

    #[test]
    fn format_report_contains_key_sections() {
        let l = build_minimal_rel(b"abc", b"x", &[("foo", 3, 1)]);
        let r = build_minimal_rel(b"abd", b"x", &[("foo", 5, 1)]);
        let rep = compare(&l, &r).unwrap();
        let s = rep.format();
        assert!(s.contains("byte diffs"));
        assert!(s.contains("symbol diffs"));
        assert!(s.contains("semantically_equivalent"));
    }

    #[test]
    fn empty_format_for_identical() {
        let elf = build_minimal_rel(b"abc", b"x", &[("foo", 3, 1)]);
        let rep = compare(&elf, &elf).unwrap();
        assert!(rep.format().contains("identical"));
    }
}
