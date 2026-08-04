// src/link/vdso.rs - vDSO 集成基础设施
//
// 第一性原理: vDSO (virtual dynamic shared object) 是内核映射到用户空间的
// 一段 ELF .so, 包含 __vdso_clock_gettime / __vdso_gettimeofday 等
// 高频系统调用替代函数. 编译器侧的 vDSO 集成要做三件事:
//
//   1) 解析 vDSO ELF (用现有 codegen::elf 工具), 提取 .text 载荷 + 符号表
//   2) 把 .text 嵌入输出 (作为 .text.vdso section), 方便运行时通过
//      AT_SYSINFO_EHDR 找到, 也方便静态分析
//   3) 暴露解析元信息 (parse + symbols API) 给 link/mod.rs 主流程
//
// 当前 commit 只完成 (1) + (3). (2) 的 .text 嵌入需要重写 layout 的
// 节添加流程, 留作下一阶段 (因为 link/mod.rs 的 layout 是按
// Vec<OutputSection> 顺序追加的, vdso 必须在所有节定位之后插入).
//
// 调用方: link/mod.rs 接收 LinkConfig.vdso: Option<PathBuf> 后, 调用
// parse_vdso() 取得 VdsoInfo, 再决定是否把 vdso 文本嵌入布局.

use std::collections::HashMap;
use std::path::Path;
use std::fs;

/// 解析后的 vDSO 信息.
#[derive(Debug, Clone)]
pub struct VdsoInfo {
    /// 源 ELF 路径 (用于诊断, 不会写入输出)
    pub source_path: String,
    /// vDSO .text 段载荷
    pub text: Vec<u8>,
    /// vDSO 符号表 (name -> (offset, size)), 只保留有 .text 偏移的 FUNC 符号
    pub symbols: HashMap<String, VdsoSymbol>,
    /// vDSO .strtab (用于调试输出)
    pub strtab: String,
    /// 整个 vDSO .text 段大小 (与 text.len() 一致, 但作为字段便于 grep)
    pub text_size: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct VdsoSymbol {
    /// 在 .text 内的偏移
    pub text_offset: u64,
    pub size: u64,
}

/// 解析失败原因.
#[derive(Debug)]
pub enum VdsoError {
    Io(String),
    BadMagic,
    NotElf64,
    Truncated(String),
    NoText,
}

impl std::fmt::Display for VdsoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VdsoError::Io(s) => write!(f, "io: {s}"),
            VdsoError::BadMagic => write!(f, "not an ELF file"),
            VdsoError::NotElf64 => write!(f, "only ELF64 vDSO supported"),
            VdsoError::Truncated(s) => write!(f, "truncated: {s}"),
            VdsoError::NoText => write!(f, "vDSO has no .text section"),
        }
    }
}

impl std::error::Error for VdsoError {}

/// 入口: 解析 vDSO ELF 文件.
/// 当前限制: 仅 ELF64 + 小端 + x86_64/aarch64/riscv64 + 至少有一个 .text 段.
pub fn parse_vdso(path: &Path) -> Result<VdsoInfo, VdsoError> {
    let buf = fs::read(path).map_err(|e| VdsoError::Io(e.to_string()))?;
    parse_vdso_bytes(&buf, path.display().to_string())
}

/// 入口: 直接对内存中的 vDSO 字节做解析 (便于测试).
pub fn parse_vdso_bytes(buf: &[u8], source: String) -> Result<VdsoInfo, VdsoError> {
    use crate::codegen::elf;

    if buf.len() < 64 {
        return Err(VdsoError::Truncated("< 64 bytes".into()));
    }
    if buf.get(0..4) != Some(&elf::ELFMAG) {
        return Err(VdsoError::BadMagic);
    }
    if buf[4] != elf::ELFCLASS64 {
        return Err(VdsoError::NotElf64);
    }

    let shoff = u64::from_le_bytes(buf.get(40..48)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| VdsoError::Truncated("e_shoff".into()))?) as usize;
    let shentsize = u16::from_le_bytes(buf.get(58..60)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| VdsoError::Truncated("e_shentsize".into()))?) as usize;
    let shnum = u16::from_le_bytes(buf.get(60..62)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| VdsoError::Truncated("e_shnum".into()))?) as usize;
    let shstrndx = u16::from_le_bytes(buf.get(62..64)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| VdsoError::Truncated("e_shstrndx".into()))?) as usize;
    if shnum == 0 || shentsize < 64 || shstrndx >= shnum {
        return Err(VdsoError::Truncated("bad shdr fields".into()));
    }

    // 1) 找 .shstrtab
    let shstr_hdr = shoff + shstrndx * shentsize;
    if shstr_hdr + 64 > buf.len() {
        return Err(VdsoError::Truncated("shstrtab header".into()));
    }
    let shstr_off = u64::from_le_bytes(buf[shstr_hdr + 24..shstr_hdr + 32]
        .try_into().unwrap()) as usize;
    let shstr_size = u64::from_le_bytes(buf[shstr_hdr + 32..shstr_hdr + 40]
        .try_into().unwrap()) as usize;
    if shstr_off + shstr_size > buf.len() {
        return Err(VdsoError::Truncated("shstrtab data".into()));
    }
    let shstrtab = &buf[shstr_off..shstr_off + shstr_size];

    // 2) 找 .text (取首个遇到的)
    let mut text_off: Option<usize> = None;
    let mut text_size: usize = 0;
    for i in 0..shnum {
        let base = shoff + i * shentsize;
        if base + 64 > buf.len() { break; }
        let name_idx = u32::from_le_bytes(buf[base..base + 4].try_into().unwrap()) as usize;
        let name = match read_cstr(shstrtab, name_idx) {
            Some(n) => n,
            None => continue,
        };
        if name == ".text" {
            let off = u64::from_le_bytes(buf[base + 24..base + 32].try_into().unwrap()) as usize;
            let sz = u64::from_le_bytes(buf[base + 32..base + 40].try_into().unwrap()) as usize;
            if off + sz > buf.len() { continue; }
            text_off = Some(off);
            text_size = sz;
            break;
        }
    }
    let text_off = text_off.ok_or(VdsoError::NoText)?;
    let text = buf[text_off..text_off + text_size].to_vec();

    // 3) 找 .symtab / .strtab (动态符号 .dynsym / .dynstr 也接受)
    let mut symtab_off: Option<(usize, usize, u64)> = None;
    let mut strtab_off: Option<(usize, usize)> = None;
    for i in 0..shnum {
        let base = shoff + i * shentsize;
        if base + 64 > buf.len() { break; }
        let name_idx = u32::from_le_bytes(buf[base..base + 4].try_into().unwrap()) as usize;
        let name = match read_cstr(shstrtab, name_idx) {
            Some(n) => n,
            None => continue,
        };
        if name == ".symtab" || name == ".dynsym" {
            if symtab_off.is_none() {
                let off = u64::from_le_bytes(buf[base + 24..base + 32].try_into().unwrap()) as usize;
                let sz = u64::from_le_bytes(buf[base + 32..base + 40].try_into().unwrap()) as usize;
                let entsize = u64::from_le_bytes(buf[base + 56..base + 64].try_into().unwrap());
                symtab_off = Some((off, sz, entsize));
            }
        } else if (name == ".strtab" || name == ".dynstr")
            && strtab_off.is_none() {
                let off = u64::from_le_bytes(buf[base + 24..base + 32].try_into().unwrap()) as usize;
                let sz = u64::from_le_bytes(buf[base + 32..base + 40].try_into().unwrap()) as usize;
                strtab_off = Some((off, sz));
            }
    }
    let (sym_off, sym_size, sym_entsize) = symtab_off
        .ok_or_else(|| VdsoError::Truncated("no symtab/dynsym".into()))?;
    let (str_off, str_size) = strtab_off
        .ok_or_else(|| VdsoError::Truncated("no strtab/dynstr".into()))?;
    if sym_entsize < 24 {
        return Err(VdsoError::Truncated("sym entsize < 24".into()));
    }
    if sym_off + sym_size > buf.len() || str_off + str_size > buf.len() {
        return Err(VdsoError::Truncated("symtab/strtab out of range".into()));
    }
    let symtab_data = &buf[sym_off..sym_off + sym_size];
    let strtab_data = &buf[str_off..str_off + str_size];
    let sym_count = sym_size / sym_entsize as usize;

    // 4) 收集所有 STT_FUNC 符号 (包括 GLOBAL / WEAK)
    let mut symbols = HashMap::new();
    for i in 0..sym_count {
        let base = i * sym_entsize as usize;
        if base + 24 > symtab_data.len() { break; }
        let st_name = u32::from_le_bytes(symtab_data[base..base + 4].try_into().unwrap()) as usize;
        let st_info = symtab_data[base + 4];
        let st_value = u64::from_le_bytes(symtab_data[base + 8..base + 16].try_into().unwrap());
        let st_size = u64::from_le_bytes(symtab_data[base + 16..base + 24].try_into().unwrap());
        let ty = st_info & 0xf;
        const STT_FUNC: u8 = 2;
        if ty != STT_FUNC { continue; }
        let name = match read_cstr(strtab_data, st_name) {
            Some(n) => n,
            None => continue,
        };
        if name.is_empty() { continue; }
        symbols.insert(name.to_string(), VdsoSymbol {
            text_offset: st_value,
            size: st_size,
        });
    }

    // 5) 把整个 strtab 转成 String (用于诊断)
    let strtab = String::from_utf8_lossy(strtab_data).to_string();

    Ok(VdsoInfo {
        source_path: source,
        text,
        symbols,
        strtab,
        text_size: text_size as u64,
    })
}

fn read_cstr(data: &[u8], off: usize) -> Option<&str> {
    let end = data.get(off..)?.iter().position(|&b| b == 0)?;
    std::str::from_utf8(&data[off..off + end]).ok()
}

/// vDSO 是否提供某个 libc 别名 (例如 clock_gettime 应当对应
/// __vdso_clock_gettime). 返回 vDSO 函数名.
pub fn vdso_alias(standard_name: &str) -> Option<&'static str> {
    // vDSO 通常暴露 __vdso_<name> 形式
    Some(match standard_name {
        "clock_gettime" => "__vdso_clock_gettime",
        "gettimeofday" => "__vdso_gettimeofday",
        "time" => "__vdso_time",
        "getcpu" => "__vdso_getcpu",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::elf;

    /// 构造一个最小 ET_DYN ELF64 (模拟 vDSO 镜像), 包含 .text + .symtab
    /// + .strtab + .shstrtab. 2 个 STT_FUNC 符号: __vdso_clock_gettime
    ///   和一个无关符号.
    fn build_minimal_vdso() -> Vec<u8> {
        let shstrtab = b"\0.text\0.symtab\0.strtab\0.shstrtab\0";
        const N_TEXT:  u32 = 1;
        const N_SYMT: u32 = 7;
        const N_STRT: u32 = 15;
        const N_SHST: u32 = 23;

        let text = b"\x90\x90\x90\x90\xc3"; // nop*4 ; ret (5 字节 .text)

        // strtab: \0 __vdso_clock_gettime \0 helper \0
        let mut strtab = vec![0u8];
        strtab.extend_from_slice(b"__vdso_clock_gettime");
        strtab.push(0);
        strtab.extend_from_slice(b"helper");
        strtab.push(0);
        let strtab_clock_off = 1usize;
        let strtab_helper_off = 1 + b"__vdso_clock_gettime".len() + 1;

        // 段布局: ELF header (64) | .text | .symtab | .strtab | .shstrtab
        let mut out = vec![0u8; 64];
        let text_off = out.len();
        out.extend_from_slice(text);
        let symtab_off = out.len();
        // 3 sym: null + __vdso_clock_gettime (FUNC, value=0) + helper (NOTYPE)
        // 每个 24B: null sym 占位
        out.resize(out.len() + 24, 0);
        // sym 1: __vdso_clock_gettime (FUNC, value=0, size=5)
        out.extend_from_slice(&(strtab_clock_off as u32).to_le_bytes());
        out.push((1 << 4) | 2); // bind=GLOBAL, type=FUNC
        out.push(0); // st_other
        out.extend_from_slice(&1u16.to_le_bytes()); // st_shndx: .text
        out.extend_from_slice(&0u64.to_le_bytes()); // st_value
        out.extend_from_slice(&5u64.to_le_bytes()); // st_size
        // sym 2: helper (FUNC, value=0, size=5)
        out.extend_from_slice(&(strtab_helper_off as u32).to_le_bytes());
        out.push((2 << 4) | 2); // bind=WEAK, type=FUNC
        out.push(0);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&5u64.to_le_bytes());

        let strtab_off = out.len();
        out.extend_from_slice(&strtab);
        let shstrtab_off = out.len();
        out.extend_from_slice(shstrtab);

        // 对齐到 8
        while !out.len().is_multiple_of(8) { out.push(0); }

        // 5 section headers (含 null)
        let sh_off = out.len();
        out.resize(out.len() + 5 * 64, 0);

        // ELF header
        out[0..4].copy_from_slice(&elf::ELFMAG);
        out[4] = elf::ELFCLASS64;
        out[5] = elf::ELFDATA2LSB;
        out[6] = elf::EV_CURRENT;
        out[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
        out[18..20].copy_from_slice(&elf::EM_X86_64.to_le_bytes());
        out[20..24].copy_from_slice(&1u32.to_le_bytes());
        out[24..32].copy_from_slice(&0u64.to_le_bytes()); // e_entry
        out[40..48].copy_from_slice(&(sh_off as u64).to_le_bytes());
        out[58..60].copy_from_slice(&64u16.to_le_bytes()); // e_shentsize
        out[60..62].copy_from_slice(&5u16.to_le_bytes()); // e_shnum
        out[62..64].copy_from_slice(&4u16.to_le_bytes()); // e_shstrndx (4)

        let write_sh = |buf: &mut Vec<u8>, idx: usize, name_off: u32,
                          sh_type: u32, flags: u64,
                          addr: u64, offset: u64, size: u64,
                          link: u32, info: u32, addralign: u64, entsize: u64| {
            let base = sh_off + idx * 64;
            buf[base..base+4].copy_from_slice(&name_off.to_le_bytes());
            buf[base+4..base+8].copy_from_slice(&sh_type.to_le_bytes());
            buf[base+8..base+16].copy_from_slice(&flags.to_le_bytes());
            buf[base+16..base+24].copy_from_slice(&addr.to_le_bytes());
            buf[base+24..base+32].copy_from_slice(&offset.to_le_bytes());
            buf[base+32..base+40].copy_from_slice(&size.to_le_bytes());
            buf[base+40..base+44].copy_from_slice(&link.to_le_bytes());
            buf[base+44..base+48].copy_from_slice(&info.to_le_bytes());
            buf[base+48..base+56].copy_from_slice(&addralign.to_le_bytes());
            buf[base+56..base+64].copy_from_slice(&entsize.to_le_bytes());
        };
        write_sh(&mut out, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        write_sh(&mut out, 1, N_TEXT, elf::SHT_PROGBITS, 0x6, 0,
                 text_off as u64, text.len() as u64, 0, 0, 1, 0);
        // .symtab: link=2 (.strtab), info=1 (first non-local sym)
        write_sh(&mut out, 2, N_SYMT, elf::SHT_SYMTAB, 0, 0,
                 symtab_off as u64, (24 * 3) as u64, 3, 1, 8, 24);
        write_sh(&mut out, 3, N_STRT, elf::SHT_STRTAB, 0, 0,
                 strtab_off as u64, strtab.len() as u64, 0, 0, 1, 0);
        write_sh(&mut out, 4, N_SHST, elf::SHT_STRTAB, 0, 0,
                 shstrtab_off as u64, shstrtab.len() as u64, 0, 0, 1, 0);

        out
    }

    #[test]
    fn parse_minimal_vdso_extracts_text_and_symbols() {
        let elf = build_minimal_vdso();
        let info = parse_vdso_bytes(&elf, "test-vdso.so".into()).unwrap();
        assert_eq!(info.text, b"\x90\x90\x90\x90\xc3");
        assert_eq!(info.text_size, 5);
        assert!(info.symbols.contains_key("__vdso_clock_gettime"));
        assert!(info.symbols.contains_key("helper"));
        let sym = info.symbols.get("__vdso_clock_gettime").unwrap();
        assert_eq!(sym.text_offset, 0);
        assert_eq!(sym.size, 5);
    }

    #[test]
    fn parse_vdso_rejects_bad_magic() {
        let mut bytes = vec![0u8; 64];
        bytes[0] = b'X';
        let r = parse_vdso_bytes(&bytes, "x".into());
        assert!(matches!(r, Err(VdsoError::BadMagic)));
    }

    #[test]
    fn parse_vdso_rejects_truncated_header() {
        let r = parse_vdso_bytes(&[0u8; 32], "x".into());
        assert!(matches!(r, Err(VdsoError::Truncated(_))));
    }

    #[test]
    fn parse_vdso_rejects_elf32() {
        let mut elf = vec![0u8; 64];
        elf[0..4].copy_from_slice(&elf::ELFMAG);
        elf[4] = 1; // ELFCLASS32
        let r = parse_vdso_bytes(&elf, "x".into());
        assert!(matches!(r, Err(VdsoError::NotElf64)));
    }

    #[test]
    fn parse_vdso_rejects_no_text() {
        // 构造一个合法 ELF64 但没有 .text 段
        let shstrtab = b"\0.shstrtab\0";
        let shstrtab_off = 64usize;
        let sh_off = shstrtab_off + shstrtab.len();
        let pad = (8 - (sh_off % 8)) % 8;
        let mut out = vec![0u8; 64];
        out.extend_from_slice(shstrtab);
        out.resize(out.len() + pad, 0);
        let sh_off = out.len();
        out.resize(out.len() + 2 * 64, 0);
        out[0..4].copy_from_slice(&elf::ELFMAG);
        out[4] = elf::ELFCLASS64;
        out[5] = elf::ELFDATA2LSB;
        out[40..48].copy_from_slice(&(sh_off as u64).to_le_bytes());
        out[58..60].copy_from_slice(&64u16.to_le_bytes());
        out[60..62].copy_from_slice(&2u16.to_le_bytes());
        out[62..64].copy_from_slice(&1u16.to_le_bytes());
        // sh[1] = .shstrtab
        let base = sh_off + 64;
        out[base..base+4].copy_from_slice(&1u32.to_le_bytes());
        out[base+4..base+8].copy_from_slice(&elf::SHT_STRTAB.to_le_bytes());
        out[base+24..base+32].copy_from_slice(&(shstrtab_off as u64).to_le_bytes());
        out[base+32..base+40].copy_from_slice(&(shstrtab.len() as u64).to_le_bytes());

        let r = parse_vdso_bytes(&out, "x".into());
        assert!(matches!(r, Err(VdsoError::NoText)));
    }

    #[test]
    fn vdso_alias_returns_expected_names() {
        assert_eq!(vdso_alias("clock_gettime"), Some("__vdso_clock_gettime"));
        assert_eq!(vdso_alias("gettimeofday"), Some("__vdso_gettimeofday"));
        assert_eq!(vdso_alias("time"), Some("__vdso_time"));
        assert_eq!(vdso_alias("getcpu"), Some("__vdso_getcpu"));
        assert_eq!(vdso_alias("unknown_func"), None);
    }

    #[test]
    fn end_to_end_parse_vdso_from_file() {
        let elf = build_minimal_vdso();
        let dir = std::env::temp_dir();
        let path = dir.join("dcc-vdso-test.so");
        std::fs::write(&path, &elf).unwrap();
        let info = parse_vdso(&path).expect("parse ok");
        assert_eq!(info.source_path, path.display().to_string());
        assert!(info.symbols.contains_key("__vdso_clock_gettime"));
        let _ = std::fs::remove_file(&path);
    }
}
