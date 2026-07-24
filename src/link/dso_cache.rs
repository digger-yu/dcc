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
        Ok(Self { mtime_nanos, size: meta.len() })
    }
}

/// 线程安全的 DSO 缓存. 零参数构造; 调用方持有并跨 link invocation 复用.
#[derive(Debug, Default)]
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
            }
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
            }
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
pub fn get_or_load<F>(cache: &mut DsoCache, path: &Path, loader: F)
    -> std::io::Result<Arc<DsoInfo>>
where
    F: FnOnce() -> std::io::Result<DsoInfo>,
{
    if let Some(c) = cache.get(path) {
        return Ok(c);
    }
    let info = loader()?;
    Ok(cache.insert(path.to_path_buf(), info))
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
        }).unwrap();
        assert_eq!(calls, 1);
        let info2 = get_or_load(&mut cache, &path, || {
            calls += 1;
            Ok(fake_info(path.clone(), 2))
        }).unwrap();
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
}
