//! ELF64 解析助手：节头 / 节数据 / 动态段 / 符号表，以及 `readelf`、`objdump -t`
//! 文本侧的取数包装。产物结构断言（自定义节、ORC、`.note`、符号尺寸）都走这里，
//! 不依赖完整 ELF 解析库。
//!
//! 由 `tests/common/harness.rs` 以 `#[path]` 挂载并 `pub(crate) use` 重导出。

use std::process::Command;

pub(crate) fn readelf_relocations(path: &std::path::Path) -> String {
    let output = Command::new("readelf")
        .args(["-r"])
        .arg(path)
        .output()
        .expect("readelf -r");
    assert!(
        output.status.success(),
        "readelf -r failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// 极简 ELF64 节头解析: 返回 (节名 → (sh_type, sh_flags, sh_size)).
/// 仅用于验证 .o 的自定义节/ORC 结构, 不依赖外部 readelf.
pub(crate) fn elf64_sections(data: &[u8]) -> std::collections::HashMap<String, (u32, u32, u64)> {
    fn read_cstr(b: &[u8]) -> String {
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).to_string()
    }
    let mut map = std::collections::HashMap::new();
    if data.len() < 64 {
        return map;
    }
    let e_shoff = u64::from_le_bytes(data[40..48].try_into().unwrap());
    let e_shentsize = u16::from_le_bytes(data[58..60].try_into().unwrap()) as usize;
    let e_shnum = u16::from_le_bytes(data[60..62].try_into().unwrap()) as usize;
    let e_shstrndx = u16::from_le_bytes(data[62..64].try_into().unwrap()) as usize;
    if e_shentsize < 64 || e_shoff as usize + e_shentsize * e_shnum > data.len() {
        return map;
    }
    let shstr = e_shoff as usize + e_shstrndx * e_shentsize;
    let shstr_off = u64::from_le_bytes(data[shstr + 24..shstr + 32].try_into().unwrap()) as usize;
    let shstr_size = u64::from_le_bytes(data[shstr + 32..shstr + 40].try_into().unwrap()) as usize;
    if shstr_off + shstr_size > data.len() {
        return map;
    }
    for i in 0..e_shnum {
        let off = e_shoff as usize + i * e_shentsize;
        let name_off = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        let ty = u32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap());
        let flags = u64::from_le_bytes(data[off + 8..off + 16].try_into().unwrap()) as u32;
        let size = u64::from_le_bytes(data[off + 32..off + 40].try_into().unwrap());
        if shstr_off + name_off < data.len() {
            let name = read_cstr(&data[shstr_off + name_off..]);
            map.insert(name, (ty, flags, size));
        }
    }
    map
}

/// 极简 ELF64 节区数据提取：按节名返回 section payload。
pub(crate) fn elf64_section_data<'a>(data: &'a [u8], section: &str) -> Option<&'a [u8]> {
    fn read_cstr(b: &[u8]) -> String {
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).to_string()
    }
    if data.len() < 64 {
        return None;
    }
    let e_shoff = u64::from_le_bytes(data[40..48].try_into().ok()?) as usize;
    let e_shentsize = u16::from_le_bytes(data[58..60].try_into().ok()?) as usize;
    let e_shnum = u16::from_le_bytes(data[60..62].try_into().ok()?) as usize;
    let e_shstrndx = u16::from_le_bytes(data[62..64].try_into().ok()?) as usize;
    if e_shentsize < 64 || e_shoff + e_shentsize * e_shnum > data.len() {
        return None;
    }
    let shstr = e_shoff + e_shstrndx * e_shentsize;
    let shstr_off = u64::from_le_bytes(data[shstr + 24..shstr + 32].try_into().ok()?) as usize;
    let shstr_size = u64::from_le_bytes(data[shstr + 32..shstr + 40].try_into().ok()?) as usize;
    if shstr_off + shstr_size > data.len() {
        return None;
    }
    for i in 0..e_shnum {
        let off = e_shoff + i * e_shentsize;
        let name_off = u32::from_le_bytes(data[off..off + 4].try_into().ok()?) as usize;
        if shstr_off + name_off >= data.len() {
            continue;
        }
        let name = read_cstr(&data[shstr_off + name_off..]);
        if name != section {
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

pub(crate) fn elf64_dynamic_needed(data: &[u8]) -> Vec<String> {
    fn read_dyn_cstr(b: &[u8]) -> String {
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).to_string()
    }
    let Some(dynamic) = elf64_section_data(data, ".dynamic") else {
        return Vec::new();
    };
    let Some(dynstr) = elf64_section_data(data, ".dynstr") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ent in dynamic.chunks_exact(16) {
        let tag = u64::from_le_bytes(ent[0..8].try_into().unwrap());
        let val = u64::from_le_bytes(ent[8..16].try_into().unwrap()) as usize;
        if tag == 0 {
            break;
        }
        if tag == 1 && val < dynstr.len() {
            out.push(read_dyn_cstr(&dynstr[val..]));
        }
    }
    out
}

/// ELF64 符号表条目 (极简解析用): 名字 + 绑定 + 类型 + 节 + 值 + 尺寸
pub(crate) struct Elf64Sym {
    pub(crate) name: String,
    /// ELF_ST_BIND(st_info): 0=LOCAL, 1=GLOBAL, 2=WEAK
    pub(crate) bind: u8,
    /// ELF_ST_TYPE(st_info): 2=FUNC, 1=OBJECT
    pub(crate) stype: u8,
    pub(crate) shndx: u16,
    pub(crate) value: u64,
    pub(crate) size: u64,
}

pub(crate) fn elf64_read_cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).to_string()
}

/// 极简 ELF64 符号表解析: 返回 `.symtab` 里的全部条目
pub(crate) fn elf64_syms(data: &[u8]) -> Vec<Elf64Sym> {
    let mut out = Vec::new();
    if data.len() < 64 {
        return out;
    }
    let e_shoff = u64::from_le_bytes(data[40..48].try_into().unwrap());
    let e_shentsize = u16::from_le_bytes(data[58..60].try_into().unwrap()) as usize;
    let e_shnum = u16::from_le_bytes(data[60..62].try_into().unwrap()) as usize;
    let e_shstrndx = u16::from_le_bytes(data[62..64].try_into().unwrap()) as usize;
    if e_shentsize < 64 || e_shoff as usize + e_shentsize * e_shnum > data.len() {
        return out;
    }
    let shstr = e_shoff as usize + e_shstrndx * e_shentsize;
    let shstr_off = u64::from_le_bytes(data[shstr + 24..shstr + 32].try_into().unwrap()) as usize;
    let shstr_size = u64::from_le_bytes(data[shstr + 32..shstr + 40].try_into().unwrap()) as usize;
    if shstr_off + shstr_size > data.len() {
        return out;
    }
    let mut symtab = None; // (offset, size, entsize)
    let mut strtab_off = 0usize;
    for i in 0..e_shnum {
        let off = e_shoff as usize + i * e_shentsize;
        let name_off = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        let name = elf64_read_cstr(&data[shstr_off + name_off..]);
        if name == ".symtab" {
            let size = u64::from_le_bytes(data[off + 32..off + 40].try_into().unwrap());
            let entsize = u64::from_le_bytes(data[off + 56..off + 64].try_into().unwrap());
            let sec_off = u64::from_le_bytes(data[off + 24..off + 32].try_into().unwrap());
            symtab = Some((sec_off, size, entsize));
        } else if name == ".strtab" {
            strtab_off = u64::from_le_bytes(data[off + 24..off + 32].try_into().unwrap()) as usize;
        }
    }
    let Some((sec_off, size, entsize)) = symtab else {
        return out;
    };
    if entsize < 24 || sec_off as usize + size as usize > data.len() {
        return out;
    }
    let n = size / entsize;
    for i in 0..n {
        let off = sec_off as usize + i as usize * entsize as usize;
        let st_name = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        let st_info = data[off + 4];
        let st_shndx = u16::from_le_bytes(data[off + 6..off + 8].try_into().unwrap());
        let st_value = u64::from_le_bytes(data[off + 8..off + 16].try_into().unwrap());
        let st_size = u64::from_le_bytes(data[off + 16..off + 24].try_into().unwrap());
        out.push(Elf64Sym {
            name: if st_name > 0 {
                elf64_read_cstr(&data[strtab_off + st_name..])
            } else {
                String::new()
            },
            bind: st_info >> 4,
            stype: st_info & 0xf,
            shndx: st_shndx,
            value: st_value,
            size: st_size,
        });
    }
    out
}

/// 定义型函数符号 (STT_FUNC, 有节) 的 (名称, st_value, st_size).
/// 用于验证函数符号 st_size 非零.
pub(crate) fn elf64_func_symbols(data: &[u8]) -> Vec<(String, u64, u64)> {
    elf64_syms(data)
        .into_iter()
        .filter(|s| s.stype == 2 && s.shndx != 0 && s.shndx != 0xfff1 && !s.name.is_empty())
        .map(|s| (s.name, s.value, s.size))
        .collect()
}

pub(crate) fn elf64_undefined_symbols(data: &[u8]) -> Vec<String> {
    elf64_syms(data)
        .into_iter()
        .filter(|s| s.shndx == 0 && !s.name.is_empty())
        .map(|s| s.name)
        .collect()
}

/// 从 `objdump -t` 文本里取名为 `name` 的对象符号尺寸。
#[cfg(unix)]
pub(crate) fn object_symbol_size(dump: &str, name: &str) -> Option<u64> {
    dump.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // 形如: `<value> <l|g|w> <O|DF> <section> <size-hex> <name>`
        match (fields.first(), fields.last()) {
            (Some(_), Some(sym)) if *sym == name => {
                if fields.len() < 5 || fields[2] != "O" {
                    return None;
                }
                let idx = fields.len() - 2;
                u64::from_str_radix(fields[idx], 16).ok()
            },
            _ => None,
        }
    })
}
