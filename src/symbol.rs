// src/symbol.rs - 字符串驻留表 (String Interner)
// 第一性原理: 标识符/字符串字面量在源码中重复率极高,
//   驻留后用 4 字节 SymbolId 替代 24+ 字节的 String, 显著降低内存与克隆开销。
// suckless 准则: 哈希表 + 互斥锁足够; 不引入第三方 crate。
//
// 设计要点:
//   * Interner 内部用 Arc<Mutex<Inner>> 共享状态, 克隆廉价 (一次原子引用计数).
//   * SymbolId 是 Copy 的 newtype, 适合作为 CompactTok 载荷.
//   * 字符串原文使用 Box::leak 取得 'static 生命周期, 保证 resolve() 返回 &str
//     时不依赖 MutexGuard 的借用. 该内存永不释放 (编译器进程内可接受).
//   * 锁定粒度为整个表 (简单够用, 高并发场景可后续细化).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 驻留字符串的唯一标识符.
///
/// 是 Copy 的 newtype, 可自由克隆并按值传递.
/// 数值 0 是占位 "空槽位"; 首个真实字符串从 1 开始计数.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub u32);

impl SymbolId {
    /// 不存在的占位 ID (供 CompactTok 默认值使用).
    pub const EMPTY: SymbolId = SymbolId(0);

    /// 内部原始值.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// 共享内部状态: 字符串池 + 反向索引表.
#[derive(Debug)]
struct Inner {
    /// 字符串原文数组 (按 SymbolId 索引).
    /// `symbols[0]` 永远为空串, 留作占位.
    /// 元素为 `&'static str`, 由 `Box::leak` 取得, 不依赖 MutexGuard 的借用.
    symbols: Vec<&'static str>,
    /// 字符串 -> SymbolId 反查表.
    map: HashMap<String, u32>,
}

impl Default for Inner {
    fn default() -> Self {
        // 索引 0 预留给空串, 避免 ID 为 0 表示"真实空字符串"引起歧义.
        Self {
            symbols: vec![""],
            map: HashMap::new(),
        }
    }
}

/// 字符串驻留器: 把 &str 映射为紧凑的 SymbolId.
///
/// 多个 Interner 之间可以共享同一个内部表 (通过 Arc), 克隆仅递增引用计数.
/// `Send + Sync` 保证可在多线程流水线 (分词/解析/常量折叠) 之间共享.
#[derive(Debug, Clone, Default)]
pub struct Interner {
    inner: Arc<Mutex<Inner>>,
}

impl Interner {
    /// 创建空的 Interner.
    pub fn new() -> Self {
        Self::default()
    }

    /// 驻留字符串并返回其 ID.
    ///
    /// 重复字符串返回相同 ID (零拷贝).
    /// 线程安全: 调用会短暂持有内部互斥锁.
    pub fn intern(&self, s: &str) -> SymbolId {
        // 快路径: 反向表已命中则无需分配.
        // 注: 即便锁内 double-check 也不会显著影响性能;
        //      此实现保持简单, 不引入 RwLock 升级.
        let mut guard = self.inner.lock().expect("interner mutex poisoned");
        if let Some(&id) = guard.map.get(s) {
            return SymbolId(id);
        }
        let id = guard.symbols.len() as u32;
        // 把字符串拷一份并泄漏成 'static, 使后续 resolve() 可返回长生命周期 &str.
        // 编译器进程内 intern 通常全程有效, 泄漏代价可接受.
        let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
        guard.symbols.push(leaked);
        guard.map.insert(s.to_owned(), id);
        SymbolId(id)
    }

    /// 按 ID 查回字符串原文.
    ///
    /// 返回的 `&str` 生命周期与 `&self` 绑定; ID=0 始终映射到空串.
    /// 越界 ID 也回退到空串, 避免 panic 影响编译流程.
    pub fn resolve(&self, id: SymbolId) -> &str {
        // 关键: 池中元素为 &'static str (由 Box::leak 取得),
        // 因此即便短暂持有 MutexGuard, 最终返回的 &str 也不借用 guard.
        let guard = self.inner.lock().expect("interner mutex poisoned");
        // 通过 copied() 把 &&'static str 转为 &'static str, 跳出 guard 借用.
        guard
            .symbols
            .get(id.0 as usize)
            .copied()
            .unwrap_or("")
    }

    /// 当前已驻留字符串总数 (含 ID=0 占位).
    pub fn len(&self) -> usize {
        let guard = self.inner.lock().expect("interner mutex poisoned");
        guard.symbols.len()
    }

    /// 是否未驻留任何字符串 (仅含 ID=0 占位).
    pub fn is_empty(&self) -> bool {
        self.len() <= 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_returns_same_id_for_same_string() {
        let it = Interner::new();
        let a = it.intern("foo");
        let b = it.intern("foo");
        assert_eq!(a, b);
    }

    #[test]
    fn intern_distinguishes_different_strings() {
        let it = Interner::new();
        let a = it.intern("foo");
        let b = it.intern("bar");
        assert_ne!(a, b);
    }

    #[test]
    fn resolve_round_trip() {
        let it = Interner::new();
        let id = it.intern("hello");
        assert_eq!(it.resolve(id), "hello");
    }

    #[test]
    fn empty_id_resolves_to_empty_string() {
        let it = Interner::new();
        assert_eq!(it.resolve(SymbolId::EMPTY), "");
    }

    #[test]
    fn interner_is_cheaply_cloneable_and_shared() {
        let a = Interner::new();
        let b = a.clone();
        let id_a = a.intern("shared");
        // 克隆端应看到同一份表 (同一 ID).
        assert_eq!(b.resolve(id_a), "shared");
        let id_b = b.intern("shared");
        assert_eq!(id_a, id_b);
    }

    #[test]
    fn out_of_range_id_returns_empty_string() {
        let it = Interner::new();
        assert_eq!(it.resolve(SymbolId(u32::MAX)), "");
    }

    #[test]
    fn len_counts_interned_strings() {
        let it = Interner::new();
        // 0 号槽位预留给空串.
        assert_eq!(it.len(), 1);
        it.intern("x");
        it.intern("y");
        assert_eq!(it.len(), 3);
        // 重复字符串不增加计数.
        it.intern("x");
        assert_eq!(it.len(), 3);
    }

    #[test]
    fn resolved_str_outlives_lock_scope() {
        // 关键不变量: resolve 返回的 &str 不能借用 MutexGuard.
        // 通过把返回值赋给独立变量并跨语句使用, 验证其生命周期独立于锁.
        let it = Interner::new();
        let id = it.intern("durable");
        let s1 = it.resolve(id);
        let s2 = it.resolve(id);
        assert_eq!(s1, "durable");
        assert_eq!(s2, "durable");
        assert_eq!(s1, s2);
    }
}