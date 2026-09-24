// src/link/dso_cache.rs - DSO 解析结果缓存
//
// 第一性原理: 内置链接器在解析 DSO (.so) 时会重复做: 打开文件 -> mmap ->
// 解析 ELF 头 -> 遍历 section headers -> 解压 .gnu_hash / .hash -> 收集
// 动态符号表. 这些对每个 link invocation 都会重做一次, 在 sysroot 多
// (musl / glibc 共存) 或 LTO 反复 link 的场景下, 累计开销不小.
//
// 本模块提供: 按 (path, mtime, size) 三元组去重的 DsoInfo 缓存. 命中
// 时直接返回 Arc<DsoInfo>, 未命中时调用回调重新解析.
//
// 设计: 缓存层不持有任何文件句柄; 走 std::fs::metadata 取 mtime/size,
// 命中后立刻 clone Arc. 这避免了 mmap 资源在多线程下的借用问题.
//
// 当前 commit 提供骨架 + 单元测试; 链接器侧接入放在后续 commit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// DSO 解析结果 (最小集, 后续按需扩展).
#[derive(Debug, Clone)]
pub struct DsoInfo {
    /// 解析的源路径
    pub path: PathBuf,
    /// .text 段载荷
    pub text: Vec<u8>,
    /// 动态符号 (STT_FUNC, dynsym): name -> (offset_in_text, size)
    pub dyn_symbols: HashMap<String, (u64, u64)>,
    /// 解析时间 (诊断用)
    pub parsed_at: SystemTime,
}

/// 缓存键: (path, mtime_unix_nanos, size)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    mtime_nanos: u64,
    size: u64,
}

impl CacheKey {
    fn from_path(p: &Path) -> std::io::Result<Self> {
        let meta = std::fs::metadata(p)?;
        let mtime_nanos = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Ok(Self {
            mtime_nanos,
            size: meta.len(),
        })
    }
}

/// 线程安全的 DSO 缓存. 零参数构造; 调用方持有并跨 link invocation 复用.
#[derive(Debug, Default, Clone)]
pub struct DsoCache {
    map: HashMap<PathBuf, (CacheKey, Arc<DsoInfo>)>,
    hits: u64,
    misses: u64,
}

impl DsoCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 命中统计 (命中率 = hits / (hits + misses)).
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// 查询缓存; 命中返回 Some(Arc), 未命中返回 None.
    /// 调用方负责实际的解析 (loader 回调) 与 insert().
    pub fn get(&mut self, path: &Path) -> Option<Arc<DsoInfo>> {
        let key = match CacheKey::from_path(path) {
            Ok(k) => k,
            Err(_) => return None,
        };
        match self.map.get(path) {
            Some((k, info)) if *k == key => {
                self.hits += 1;
                Some(info.clone())
            },
            _ => None,
        }
    }

    /// 解析后插入缓存. 失败时不上缓存 (让下一次重试).
    pub fn insert(&mut self, path: PathBuf, info: DsoInfo) -> Arc<DsoInfo> {
        let key = match CacheKey::from_path(&path) {
            Ok(k) => k,
            Err(_) => {
                // 无 metadata, 不上缓存. 调用方再调用 get() 也会失败.
                self.misses += 1;
                return Arc::new(info);
            },
        };
        self.misses += 1;
        let arc = Arc::new(info);
        self.map.insert(path, (key, arc.clone()));
        arc
    }

    /// 清空缓存 (例如在 -Bdynamic / -Bstatic 切换时).
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// 当前缓存条目数.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 拿到缓存 (用于诊断打印 / debug 输出).
    pub fn cached_paths(&self) -> Vec<PathBuf> {
        self.map.keys().cloned().collect()
    }
}

/// 便捷入口: get_or_load 包装; loader 失败时返回 io::Error.
pub fn get_or_load<F>(cache: &mut DsoCache, path: &Path, loader: F) -> std::io::Result<Arc<DsoInfo>>
where
    F: FnOnce() -> std::io::Result<DsoInfo>,
{
    if let Some(c) = cache.get(path) {
        return Ok(c);
    }
    let info = loader()?;
    Ok(cache.insert(path.to_path_buf(), info))
}

/// 在搜索路径中查找 -lxxx 对应的 .so 文件.
///
/// 搜索顺序:
///   1. 显式搜索路径 (from -L flags)
///   2. 标准系统路径: /usr/lib, /lib, /usr/lib64, /lib64
///   3. sysroot 前缀路径 (如果提供)
///
/// 返回首个找到的文件路径.
pub fn resolve_library(
    lib_name: &str,
    search_paths: &[PathBuf],
    sysroot: Option<&Path>,
) -> Option<PathBuf> {
    let filename = format!("lib{}.so", lib_name);

    // 1. 显式搜索路径
    for dir in search_paths {
        let candidate = dir.join(&filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 2. sysroot 前缀
    if let Some(sr) = sysroot {
        let sr_lib_dirs = [
            sr.join("lib"),
            sr.join("lib64"),
            sr.join("usr/lib"),
            sr.join("usr/lib64"),
        ];
        for dir in &sr_lib_dirs {
            let candidate = dir.join(&filename);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // 3. 标准系统路径
    let system_dirs = [
        "/usr/lib",
        "/lib",
        "/usr/lib64",
        "/lib64",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
    ];
    for dir in &system_dirs {
        let candidate = Path::new(dir).join(&filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

/// 解析 .so 文件为 DsoInfo (提取动态符号表).
pub fn parse_dso_file(path: &Path) -> std::io::Result<DsoInfo> {
    use crate::codegen::shared::elf;

    let buf = std::fs::read(path)?;
    if buf.len() < 64 || buf.get(0..4) != Some(&elf::ELFMAG) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: not a valid ELF file", path.display()),
        ));
    }

    let shoff = u64::from_le_bytes(
        buf.get(40..48)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 8]),
    ) as usize;
    let shentsize = u16::from_le_bytes(
        buf.get(58..60)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    let shnum = u16::from_le_bytes(
        buf.get(60..62)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    let shstrndx = u16::from_le_bytes(
        buf.get(62..64)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;

    if shnum == 0 || shentsize < 64 || shstrndx >= shnum || shoff + shnum * shentsize > buf.len() {
        return Ok(DsoInfo {
            path: path.to_path_buf(),
            text: Vec::new(),
            dyn_symbols: HashMap::new(),
            parsed_at: SystemTime::now(),
        });
    }

    // 找 .shstrtab
    let shstr_hdr = shoff + shstrndx * shentsize;
    let shstr_off =
        u64::from_le_bytes(buf[shstr_hdr + 24..shstr_hdr + 32].try_into().unwrap()) as usize;
    let shstr_size =
        u64::from_le_bytes(buf[shstr_hdr + 32..shstr_hdr + 40].try_into().unwrap()) as usize;
    if shstr_off + shstr_size > buf.len() {
        return Ok(DsoInfo {
            path: path.to_path_buf(),
            text: Vec::new(),
            dyn_symbols: HashMap::new(),
            parsed_at: SystemTime::now(),
        });
    }
    let shstrtab = &buf[shstr_off..shstr_off + shstr_size];

    // 找 .dynsym / .dynstr (或 .symtab / .strtab)
    let mut symtab_off: Option<(usize, usize, u64)> = None;
    let mut strtab_off: Option<(usize, usize)> = None;
    let mut text_off: Option<(usize, usize)> = None;

    for i in 0..shnum {
        let base = shoff + i * shentsize;
        if base + 64 > buf.len() {
            break;
        }
        let name_idx = u32::from_le_bytes(buf[base..base + 4].try_into().unwrap()) as usize;
        let name = read_cstr_from(shstrtab, name_idx).unwrap_or("");
        match name {
            ".dynsym" | ".symtab" if symtab_off.is_none() => {
                let off =
                    u64::from_le_bytes(buf[base + 24..base + 32].try_into().unwrap()) as usize;
                let sz = u64::from_le_bytes(buf[base + 32..base + 40].try_into().unwrap()) as usize;
                let entsize = u64::from_le_bytes(buf[base + 56..base + 64].try_into().unwrap());
                symtab_off = Some((off, sz, entsize));
            },
            ".dynstr" | ".strtab" if strtab_off.is_none() => {
                let off =
                    u64::from_le_bytes(buf[base + 24..base + 32].try_into().unwrap()) as usize;
                let sz = u64::from_le_bytes(buf[base + 32..base + 40].try_into().unwrap()) as usize;
                strtab_off = Some((off, sz));
            },
            ".text" if text_off.is_none() => {
                let off =
                    u64::from_le_bytes(buf[base + 24..base + 32].try_into().unwrap()) as usize;
                let sz = u64::from_le_bytes(buf[base + 32..base + 40].try_into().unwrap()) as usize;
                text_off = Some((off, sz));
            },
            _ => {},
        }
    }

    let text = if let Some((off, sz)) = text_off {
        if off + sz <= buf.len() {
            buf[off..off + sz].to_vec()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let mut dyn_symbols = HashMap::new();
    if let (Some((sym_off, sym_size, sym_entsize)), Some((str_off, str_size))) =
        (symtab_off, strtab_off)
    {
        if sym_entsize >= 24 && sym_off + sym_size <= buf.len() && str_off + str_size <= buf.len() {
            let symtab_data = &buf[sym_off..sym_off + sym_size];
            let strtab_data = &buf[str_off..str_off + str_size];
            let sym_count = sym_size / sym_entsize as usize;

            for i in 0..sym_count {
                let base = i * sym_entsize as usize;
                if base + 24 > symtab_data.len() {
                    break;
                }
                let st_name =
                    u32::from_le_bytes(symtab_data[base..base + 4].try_into().unwrap()) as usize;
                let st_info = symtab_data[base + 4];
                let st_value =
                    u64::from_le_bytes(symtab_data[base + 8..base + 16].try_into().unwrap());
                let st_size =
                    u64::from_le_bytes(symtab_data[base + 16..base + 24].try_into().unwrap());
                let ty = st_info & 0xf;
                // 只收集 FUNC 符号
                if ty != 2 {
                    // STT_FUNC = 2
                    continue;
                }
                if let Some(name) = read_cstr_from(strtab_data, st_name) {
                    if !name.is_empty() {
                        dyn_symbols.insert(name.to_string(), (st_value, st_size));
                    }
                }
            }
        }
    }

    Ok(DsoInfo {
        path: path.to_path_buf(),
        text,
        dyn_symbols,
        parsed_at: SystemTime::now(),
    })
}

/// 结合搜索路径解析并缓存 -lxxx.
pub fn resolve_and_load(
    cache: &mut DsoCache,
    lib_name: &str,
    search_paths: &[PathBuf],
    sysroot: Option<&Path>,
) -> std::io::Result<Arc<DsoInfo>> {
    let path = resolve_library(lib_name, search_paths, sysroot).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("cannot find -l{} in search paths", lib_name),
        )
    })?;
    get_or_load(cache, &path, || parse_dso_file(&path))
}

fn read_cstr_from(data: &[u8], off: usize) -> Option<&str> {
    let end = data.get(off..)?.iter().position(|&b| b == 0)?;
    std::str::from_utf8(&data[off..off + end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_dso(content: &[u8], name: &str) -> PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
        path
    }

    fn fake_info(path: PathBuf, n: u32) -> DsoInfo {
        DsoInfo {
            path,
            text: vec![0x90; n as usize],
            dyn_symbols: HashMap::new(),
            parsed_at: SystemTime::now(),
        }
    }

    #[test]
    fn cache_miss_then_hit() {
        let path = write_temp_dso(b"fake-dso-v1", "dcc-dso-cache-1.so");
        let mut cache = DsoCache::new();
        assert!(cache.get(&path).is_none());
        cache.insert(path.clone(), fake_info(path.clone(), 16));
        let cached = cache.get(&path);
        assert!(cached.is_some());
        let (hits, misses) = cache.stats();
        assert_eq!(hits, 1);
        assert_eq!(misses, 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cache_invalidates_on_size_change() {
        let path = write_temp_dso(b"small", "dcc-dso-cache-2.so");
        let mut cache = DsoCache::new();
        cache.insert(path.clone(), fake_info(path.clone(), 5));
        assert!(cache.get(&path).is_some());
        // 改文件 size
        std::fs::write(&path, b"much larger content now").unwrap();
        // metadata 变化, 应 cache miss
        assert!(cache.get(&path).is_none());
        let (hits, misses) = cache.stats();
        assert_eq!(hits, 1);
        assert_eq!(misses, 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn get_or_load_calls_loader_only_on_miss() {
        let path = write_temp_dso(b"x", "dcc-dso-cache-3.so");
        let mut cache = DsoCache::new();
        let mut calls = 0;
        let info = get_or_load(&mut cache, &path, || {
            calls += 1;
            Ok(fake_info(path.clone(), 1))
        })
        .unwrap();
        assert_eq!(calls, 1);
        let info2 = get_or_load(&mut cache, &path, || {
            calls += 1;
            Ok(fake_info(path.clone(), 2))
        })
        .unwrap();
        // 第二次走缓存, loader 不应被调用
        assert_eq!(calls, 1);
        // 两次返回相同 Arc 内容
        assert_eq!(info.text, info2.text);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn clear_empties_cache() {
        let path = write_temp_dso(b"y", "dcc-dso-cache-4.so");
        let mut cache = DsoCache::new();
        cache.insert(path.clone(), fake_info(path.clone(), 1));
        assert_eq!(cache.len(), 1);
        cache.clear();
        assert!(cache.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_path_returns_none() {
        let mut cache = DsoCache::new();
        let bogus = std::env::temp_dir().join("dcc-dso-cache-does-not-exist.so");
        let _ = std::fs::remove_file(&bogus);
        assert!(cache.get(&bogus).is_none());
    }

    #[test]
    fn resolve_library_search_paths() {
        let dir = std::env::temp_dir().join("dcc-dso-resolve-test");
        let _ = std::fs::create_dir_all(&dir);
        let so_path = dir.join("libfoo.so");
        std::fs::write(&so_path, b"fake-so").unwrap();

        let result = resolve_library("foo", std::slice::from_ref(&dir), None);
        assert!(result.is_some(), "should find libfoo.so in search path");
        assert_eq!(result.unwrap(), so_path);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_library_not_found() {
        let result = resolve_library(
            "nonexistent_lib_xyz_12345",
            &[PathBuf::from("/nonexistent/path")],
            None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn resolve_and_load_uses_cache() {
        let dir = std::env::temp_dir().join("dcc-dso-resolve-load-test");
        let _ = std::fs::create_dir_all(&dir);
        // 构造最小 ELF
        let mut elf = vec![0u8; 64];
        elf[0..4].copy_from_slice(&crate::codegen::shared::elf::ELFMAG);
        elf[4] = crate::codegen::shared::elf::ELFCLASS64;
        elf[5] = crate::codegen::shared::elf::ELFDATA2LSB;
        elf[6] = crate::codegen::shared::elf::EV_CURRENT;
        let so_path = dir.join("libbar.so");
        std::fs::write(&so_path, &elf).unwrap();

        let mut cache = DsoCache::new();
        let result = resolve_and_load(&mut cache, "bar", std::slice::from_ref(&dir), None);
        assert!(result.is_ok(), "should load: {:?}", result.err());

        // 第二次应走缓存
        let (hits_before, misses_before) = cache.stats();
        let result2 = resolve_and_load(&mut cache, "bar", std::slice::from_ref(&dir), None);
        assert!(result2.is_ok());
        let (hits_after, misses_after) = cache.stats();
        assert_eq!(hits_after, hits_before + 1, "should be a cache hit");
        assert_eq!(misses_after, misses_before, "no new miss");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
