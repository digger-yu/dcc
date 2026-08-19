// src/profile/memprof.rs - MemProf / Page Weight profiler
//
// MemProf 是 LLVM 18+ 的内存采样器, 与 PGO/AutoFDO 并列, 但关注
// 堆分配的生命周期与占用. 主要用途:
//   * `-fmemory-profile` : 在 malloc/free/operator new 路径上打点,
//     收集 (alloc_site, dealloc_site, lifetime, size) -> shadow
//   * `-fmemory-profile-use=file` : 读取 .memprofdata, 把
//     `cold_alloc` / `not_lifetime_extended` / `hinted` 等属性注入
//     到 IR (通常是 `__attribute__((malloc("cold")))`)
//
// dcc 文本格式:
//   # dcc memprof v1
//   alloc <frames...> = <count> <bytes>
//   lifetime <alloc_frames> -> <dealloc_frames> = <bytes>
//   dealloc <frames...> = <count>
//
// dcc 二进制格式 (.memprofdata):
//   magic    : "MEMPROFD" 8 bytes
//   version  : u32 LE (1)
//   n_allocs : u32 LE
//   alloc[]  : n_frames(u32) frames[](len(u32) + bytes) count(u64) bytes(u64)
//   n_life   : u32 LE
//   life[]   : alloc_frames(...) dealloc_frames(...) lifetime_bytes(u64)
//   n_deallocs: u32 LE
//   dealloc[]: n_frames(u32) frames(...) count(u64)
//
// malloc 插桩 (分配器拦截):
//   codegen 把 `malloc(size)` 改写为 `__dcc_memprof_alloc_record(size,
//   return_addr)` (返回指针), 把 `free(p)` 改写为
//   `__dcc_memprof_free_record(p, return_addr)`
//   runtime 库收集 callstack + size + lifetime 数据, atexit 写
//   `.memprofdata` (二进制格式见上)。
//
// 已接入 driver → ProfileWeights → codegen (cold 函数下沉 .text.unlikely).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// MemProf 配置
#[derive(Debug, Clone, Default)]
pub struct MemProfConfig {
    /// `-fmemory-profile` (instrument)
    pub instrument: bool,
    /// `-fmemory-profile-use=file`
    pub use_file: Option<String>,
    /// cold 分配阈值 (alloc count < 此值视为 cold)
    pub cold_threshold: u64,
}

impl MemProfConfig {
    pub fn new() -> Self {
        Self {
            cold_threshold: 5,
            ..Default::default()
        }
    }
}

/// 分配分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocClass {
    /// 高频分配 (count >= hot_threshold)
    Hot,
    /// 正常分配
    Normal,
    /// 低频分配 (count < cold_threshold)
    Cold,
    /// 短命分配 (alloc 到 dealloc 极快)
    Ephemeral,
}

impl AllocClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            AllocClass::Hot => "hot",
            AllocClass::Normal => "normal",
            AllocClass::Cold => "cold",
            AllocClass::Ephemeral => "ephemeral",
        }
    }
}

/// MemProf 数据
#[derive(Debug, Clone, Default)]
pub struct MemProfData {
    /// callstack -> 分配次数
    pub alloc_counts: HashMap<Vec<String>, u64>,
    /// callstack -> 累计字节
    pub alloc_bytes: HashMap<Vec<String>, u64>,
    /// (alloc_stack, dealloc_stack) -> 累计字节
    pub lifetime: HashMap<(Vec<String>, Vec<String>), u64>,
    /// dealloc callstack -> 释放次数
    pub dealloc_counts: HashMap<Vec<String>, u64>,
}

/// MemProf 概要统计
#[derive(Debug, Clone, Default)]
pub struct MemProfSummary {
    pub total_allocs: u64,
    pub total_bytes: u64,
    pub unique_alloc_sites: usize,
    pub unique_dealloc_sites: usize,
    pub lifetime_entries: usize,
    pub hot_sites: usize,
    pub cold_sites: usize,
    pub ephemeral_sites: usize,
}

impl MemProfData {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从文件路径解析 (自动检测文本/二进制格式)
    pub fn read(path: &Path) -> io::Result<Self> {
        let data = fs::read(path)?;
        if data.len() >= 8 && &data[0..8] == b"MEMPROFD" {
            return Self::read_binary(&data);
        }
        let s =
            String::from_utf8(data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Self::from_str(&s)
    }

    /// 从字符串解析 dcc 简化文本格式:
    ///   `alloc <frames...> = <count> <bytes>`
    ///   `lifetime <alloc_frames...> -> <dealloc_frames...> = <bytes>`
    ///   `dealloc <frames...> = <count>`
    pub fn from_str(input: &str) -> io::Result<Self> {
        let mut d = Self::default();
        for line in input.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            if let Some(rest) = l.strip_prefix("alloc ") {
                if let Some((frames, tail)) = rest.split_once(" = ") {
                    let stack: Vec<String> = frames.split(" -> ").map(|s| s.to_string()).collect();
                    let mut it = tail.split_whitespace();
                    let count: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let bytes: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    *d.alloc_counts.entry(stack.clone()).or_insert(0) += count;
                    *d.alloc_bytes.entry(stack).or_insert(0) += bytes;
                }
            } else if let Some(rest) = l.strip_prefix("lifetime ") {
                if let Some((pair, tail)) = rest.split_once(" = ") {
                    // 用 " => " 分隔 alloc_stack 和 dealloc_stack
                    // (内部帧用 " -> " 分隔)
                    if let Some((af, df)) = pair.split_once(" => ") {
                        let a: Vec<String> = af.split(" -> ").map(|s| s.to_string()).collect();
                        let de: Vec<String> = df.split(" -> ").map(|s| s.to_string()).collect();
                        let bytes: u64 = tail.trim().parse().unwrap_or(0);
                        *d.lifetime.entry((a, de)).or_insert(0) += bytes;
                    }
                }
            } else if let Some(rest) = l.strip_prefix("dealloc ") {
                if let Some((frames, tail)) = rest.split_once(" = ") {
                    let stack: Vec<String> = frames.split(" -> ").map(|s| s.to_string()).collect();
                    let count: u64 = tail.trim().parse().unwrap_or(0);
                    *d.dealloc_counts.entry(stack).or_insert(0) += count;
                }
            }
        }
        Ok(d)
    }

    /// 给定分配点 callstack, 返回 `is_cold` 启发 (alloc count < 阈值)
    pub fn is_cold(&self, stack: &[String], threshold: u64) -> bool {
        self.alloc_counts.get(stack).copied().unwrap_or(0) < threshold
    }

    /// 分类分配点
    pub fn classify(
        &self,
        stack: &[String],
        cold_threshold: u64,
        hot_threshold: u64,
    ) -> AllocClass {
        let count = self.alloc_counts.get(stack).copied().unwrap_or(0);
        // 检查是否 ephemeral (有极短 lifetime)
        let is_ephemeral = self.lifetime.iter().any(|((a, _), bytes)| {
            a == stack && *bytes < 64 // 小于 64 字节视为短命
        });
        if is_ephemeral && count < cold_threshold * 2 {
            return AllocClass::Ephemeral;
        }
        if count >= hot_threshold {
            AllocClass::Hot
        } else if count < cold_threshold {
            AllocClass::Cold
        } else {
            AllocClass::Normal
        }
    }

    /// 合并另一个 profile 到自身
    pub fn merge(&mut self, other: &MemProfData) {
        for (k, &v) in &other.alloc_counts {
            *self.alloc_counts.entry(k.clone()).or_insert(0) += v;
        }
        for (k, &v) in &other.alloc_bytes {
            *self.alloc_bytes.entry(k.clone()).or_insert(0) += v;
        }
        for (k, &v) in &other.lifetime {
            *self.lifetime.entry(k.clone()).or_insert(0) += v;
        }
        for (k, &v) in &other.dealloc_counts {
            *self.dealloc_counts.entry(k.clone()).or_insert(0) += v;
        }
    }

    /// 概要统计
    pub fn summary(&self) -> MemProfSummary {
        let cold_threshold = 5;
        let hot_threshold = 100;
        let mut hot_sites = 0;
        let mut cold_sites = 0;
        let mut ephemeral_sites = 0;
        for stack in self.alloc_counts.keys() {
            match self.classify(stack, cold_threshold, hot_threshold) {
                AllocClass::Hot => hot_sites += 1,
                AllocClass::Cold => cold_sites += 1,
                AllocClass::Ephemeral => ephemeral_sites += 1,
                AllocClass::Normal => {}
            }
        }
        MemProfSummary {
            total_allocs: self.alloc_counts.values().copied().sum(),
            total_bytes: self.alloc_bytes.values().copied().sum(),
            unique_alloc_sites: self.alloc_counts.len(),
            unique_dealloc_sites: self.dealloc_counts.len(),
            lifetime_entries: self.lifetime.len(),
            hot_sites,
            cold_sites,
            ephemeral_sites,
        }
    }

    /// 获取所有分配点的 callstack 列表
    pub fn alloc_sites(&self) -> Vec<&Vec<String>> {
        self.alloc_counts.keys().collect()
    }

    /// 获取某分配点的字节数
    pub fn bytes_for_site(&self, stack: &[String]) -> u64 {
        self.alloc_bytes.get(stack).copied().unwrap_or(0)
    }

    /// 序列化为 dcc 文本格式
    pub fn serialize(&self) -> String {
        let mut out = String::from("# dcc memprof v1\n");
        for (stack, &count) in &self.alloc_counts {
            let frames = stack.join(" -> ");
            let bytes = self.alloc_bytes.get(stack).copied().unwrap_or(0);
            out.push_str(&format!("alloc {} = {} {}\n", frames, count, bytes));
        }
        for ((a, d), &bytes) in &self.lifetime {
            let af = a.join(" -> ");
            let df = d.join(" -> ");
            out.push_str(&format!("lifetime {} => {} = {}\n", af, df, bytes));
        }
        for (stack, &count) in &self.dealloc_counts {
            let frames = stack.join(" -> ");
            out.push_str(&format!("dealloc {} = {}\n", frames, count));
        }
        out
    }

    /// 序列化为 dcc 二进制格式 (.memprofdata)
    pub fn write_binary(&self, out: &Path) -> io::Result<usize> {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"MEMPROFD");
        buf.extend_from_slice(&1u32.to_le_bytes()); // version
                                                    // allocs
        buf.extend_from_slice(&(self.alloc_counts.len() as u32).to_le_bytes());
        for (stack, &count) in &self.alloc_counts {
            buf.extend_from_slice(&(stack.len() as u32).to_le_bytes());
            for frame in stack {
                let bytes = frame.as_bytes();
                buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                buf.extend_from_slice(bytes);
            }
            buf.extend_from_slice(&count.to_le_bytes());
            let bytes = self.alloc_bytes.get(stack).copied().unwrap_or(0);
            buf.extend_from_slice(&bytes.to_le_bytes());
        }
        // lifetime
        buf.extend_from_slice(&(self.lifetime.len() as u32).to_le_bytes());
        for ((a, d), &bytes) in &self.lifetime {
            buf.extend_from_slice(&(a.len() as u32).to_le_bytes());
            for frame in a {
                let fb = frame.as_bytes();
                buf.extend_from_slice(&(fb.len() as u32).to_le_bytes());
                buf.extend_from_slice(fb);
            }
            buf.extend_from_slice(&(d.len() as u32).to_le_bytes());
            for frame in d {
                let fb = frame.as_bytes();
                buf.extend_from_slice(&(fb.len() as u32).to_le_bytes());
                buf.extend_from_slice(fb);
            }
            buf.extend_from_slice(&bytes.to_le_bytes());
        }
        // deallocs
        buf.extend_from_slice(&(self.dealloc_counts.len() as u32).to_le_bytes());
        for (stack, &count) in &self.dealloc_counts {
            buf.extend_from_slice(&(stack.len() as u32).to_le_bytes());
            for frame in stack {
                let bytes = frame.as_bytes();
                buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                buf.extend_from_slice(bytes);
            }
            buf.extend_from_slice(&count.to_le_bytes());
        }

        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = fs::create_dir_all(parent);
            }
        }
        fs::write(out, &buf)?;
        Ok(buf.len())
    }

    /// 从 dcc 二进制格式解析
    pub fn read_binary(data: &[u8]) -> io::Result<Self> {
        if data.len() < 12 || &data[0..8] != b"MEMPROFD" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad memprofdata magic",
            ));
        }
        let _version = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let mut off = 12;
        let mut d = Self::default();
        // allocs
        let n_allocs = read_u32(data, &mut off)? as usize;
        for _ in 0..n_allocs {
            let n_frames = read_u32(data, &mut off)? as usize;
            let mut stack = Vec::with_capacity(n_frames);
            for _ in 0..n_frames {
                let frame = read_string(data, &mut off)?;
                stack.push(frame);
            }
            let count = read_u64(data, &mut off)?;
            let bytes = read_u64(data, &mut off)?;
            *d.alloc_counts.entry(stack.clone()).or_insert(0) += count;
            *d.alloc_bytes.entry(stack).or_insert(0) += bytes;
        }
        // lifetime
        let n_life = read_u32(data, &mut off)? as usize;
        for _ in 0..n_life {
            let n_a = read_u32(data, &mut off)? as usize;
            let mut a = Vec::with_capacity(n_a);
            for _ in 0..n_a {
                a.push(read_string(data, &mut off)?);
            }
            let n_d = read_u32(data, &mut off)? as usize;
            let mut de = Vec::with_capacity(n_d);
            for _ in 0..n_d {
                de.push(read_string(data, &mut off)?);
            }
            let bytes = read_u64(data, &mut off)?;
            *d.lifetime.entry((a, de)).or_insert(0) += bytes;
        }
        // deallocs
        let n_deallocs = read_u32(data, &mut off)? as usize;
        for _ in 0..n_deallocs {
            let n_frames = read_u32(data, &mut off)? as usize;
            let mut stack = Vec::with_capacity(n_frames);
            for _ in 0..n_frames {
                stack.push(read_string(data, &mut off)?);
            }
            let count = read_u64(data, &mut off)?;
            *d.dealloc_counts.entry(stack).or_insert(0) += count;
        }
        Ok(d)
    }
}

/// malloc 插桩器: 生成对 runtime 库函数的调用
pub struct MemProfInstrumentor;

impl MemProfInstrumentor {
    /// 返回替换 malloc 调用的表达式。
    /// `__dcc_memprof_alloc_record(size, return_addr)` 内部调用 libc
    /// malloc 并返回指针, 因此插桩直接把 `malloc(x)` 改写为该调用。
    pub fn alloc_record_call(size_expr: &str, return_addr_expr: &str) -> String {
        format!(
            "__dcc_memprof_alloc_record({}, {})",
            size_expr, return_addr_expr
        )
    }

    /// 返回替换 free 调用的调用。
    /// `__dcc_memprof_free_record(ptr, return_addr)` 记录 dealloc site
    /// 与 lifetime 后自行调用 libc free, 因此插桩直接把 `free(p)`
    /// 改写为该调用。
    pub fn free_record_call(ptr_expr: &str, return_addr_expr: &str) -> String {
        format!(
            "__dcc_memprof_free_record({}, {})",
            ptr_expr, return_addr_expr
        )
    }

    /// 返回需要链接的 runtime 函数列表
    pub fn runtime_symbols() -> Vec<&'static str> {
        vec![
            "__dcc_memprof_alloc_record",
            "__dcc_memprof_free_record",
            "__dcc_memprof_init",
            "__dcc_memprof_flush",
        ]
    }
}

/// MemProf runtime 库接口定义
pub struct MemProfRuntime;

impl MemProfRuntime {
    /// 生成 runtime 库的 C 代码 (用于链接)。
    ///
    /// 与 GCOV/PGO 运行时不同, MemProf 采用 "分配器拦截" 模式 (与
    /// LLVM MemProf / ASan 一致): 插桩把 `malloc(size)` 改写为
    /// `__dcc_memprof_alloc_record(size, ret)` (返回指针), 把
    /// `free(p)` 改写为 `__dcc_memprof_free_record(p, ret)`。运行时
    /// 内部直接调用 libc 的 malloc/free, 不会被二次插桩。
    ///
    /// 记录的数据:
    ///   * alloc site  (callstack -> count, bytes)
    ///   * dealloc site (callstack -> count)
    ///   * lifetime   ((alloc_stack, dealloc_stack) -> bytes, 由
    ///     active 分配表把 free 配对回对应的 alloc site)
    ///
    /// 全部使用静态数组 (无 malloc), 避免与插桩代码互相递归。
    /// `.memprofdata` 二进制布局与 `MemProfData::write_binary` /
    /// `read_binary` 完全一致, 可由 `-fmemory-profile-use=` 读回。
    /// 输出路径: `DCC_MEMPROF_OUT` 环境变量, 缺省当前目录
    /// `memprof.memprofdata`。
    pub fn generate_runtime() -> String {
        r#"/* dcc memprof runtime - generated code */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define _MP_MAX_STACK    8
#define _MP_MAX_SITES    256
#define _MP_MAX_ACTIVE   256
#define _MP_MAX_LIFE     512
#define _MP_MAX_DEALLOC  256

static int _dcc_memprof_initialized = 0;

struct _mp_stack {
    int n;
    uint64_t frames[_MP_MAX_STACK];
};

struct _mp_site {
    struct _mp_stack stack;
    uint64_t count;
    uint64_t bytes;
    int in_use;
};

struct _mp_active {
    void *ptr;
    uint64_t size;
    int site_idx;
    int in_use;
};

struct _mp_life {
    int a_idx;
    int d_idx;
    uint64_t bytes;
    int in_use;
};

struct _mp_dealloc {
    struct _mp_stack stack;
    uint64_t count;
    int in_use;
};

static struct _mp_site _mp_sites[_MP_MAX_SITES];
static struct _mp_active _mp_active[_MP_MAX_ACTIVE];
static struct _mp_life _mp_life[_MP_MAX_LIFE];
static struct _mp_dealloc _mp_deallocs[_MP_MAX_DEALLOC];

static void _mp_stack_capture(struct _mp_stack *s, void *ret) {
    s->n = 0;
    s->frames[s->n++] = (uint64_t)(uintptr_t)ret;
    /*
     * 手动帧指针回绕 (x86_64 SysV: [fp] = 前驱 fp, [fp+8] = 返回地址)。
     * 不能用 __builtin_return_address(N>0) 链式回绕: dcc 生成的调用方
     * 用 r13 而非 rbp 作帧指针, 链式回绕会读到垃圾地址并段错误。
     * 这里用 "新 fp 严格递减 + 步长 <= 4MB" 双重约束保证安全。
     */
#if defined(__x86_64__)
    void **fp = (void **)__builtin_frame_address(0);
    while (s->n < _MP_MAX_STACK && fp) {
        void *ra = fp[1];
        void *nxt = fp[0];
        if (!ra || !nxt) break;
        if ((uintptr_t)nxt >= (uintptr_t)fp) break;
        if ((uintptr_t)fp - (uintptr_t)nxt > (uintptr_t)(4 * 1024 * 1024)) break;
        s->frames[s->n++] = (uint64_t)(uintptr_t)ra;
        fp = (void **)nxt;
    }
#endif
}

static uint64_t _mp_stack_hash(const struct _mp_stack *s) {
    uint64_t h = 1469598103934665603ull; /* FNV-1a offset basis */
    for (int i = 0; i < s->n; i++) {
        uint64_t f = s->frames[i];
        for (int b = 0; b < 8; b++) {
            h ^= (f >> (8 * b)) & 0xff;
            h *= 1099511628211ull; /* FNV-1a prime */
        }
    }
    h ^= (uint64_t)s->n;
    return h;
}

static int _mp_stack_eq(const struct _mp_stack *a, const struct _mp_stack *b) {
    if (a->n != b->n) return 0;
    return memcmp(a->frames, b->frames, sizeof(uint64_t) * (size_t)a->n) == 0;
}

static int _mp_find_site(const struct _mp_stack *s) {
    uint64_t h = _mp_stack_hash(s);
    for (int i = 0; i < _MP_MAX_SITES; i++) {
        if (!_mp_sites[i].in_use) continue;
        if (_mp_stack_hash(&_mp_sites[i].stack) != h) continue;
        if (_mp_stack_eq(&_mp_sites[i].stack, s)) return i;
    }
    return -1;
}

static int _mp_new_site(const struct _mp_stack *s) {
    for (int i = 0; i < _MP_MAX_SITES; i++) {
        if (!_mp_sites[i].in_use) {
            _mp_sites[i].stack = *s;
            _mp_sites[i].count = 0;
            _mp_sites[i].bytes = 0;
            _mp_sites[i].in_use = 1;
            return i;
        }
    }
    return -1;
}

static int _mp_find_dealloc(const struct _mp_stack *s) {
    for (int i = 0; i < _MP_MAX_DEALLOC; i++) {
        if (!_mp_deallocs[i].in_use) continue;
        if (_mp_stack_eq(&_mp_deallocs[i].stack, s)) return i;
    }
    return -1;
}

static int _mp_new_dealloc(const struct _mp_stack *s) {
    for (int i = 0; i < _MP_MAX_DEALLOC; i++) {
        if (!_mp_deallocs[i].in_use) {
            _mp_deallocs[i].stack = *s;
            _mp_deallocs[i].count = 0;
            _mp_deallocs[i].in_use = 1;
            return i;
        }
    }
    return -1;
}

static void _mp_record_lifetime(int a_idx, int d_idx, uint64_t bytes) {
    for (int i = 0; i < _MP_MAX_LIFE; i++) {
        if (_mp_life[i].in_use && _mp_life[i].a_idx == a_idx
            && _mp_life[i].d_idx == d_idx) {
            _mp_life[i].bytes += bytes;
            return;
        }
    }
    for (int i = 0; i < _MP_MAX_LIFE; i++) {
        if (!_mp_life[i].in_use) {
            _mp_life[i].a_idx = a_idx;
            _mp_life[i].d_idx = d_idx;
            _mp_life[i].bytes = bytes;
            _mp_life[i].in_use = 1;
            return;
        }
    }
}

void __dcc_memprof_init(void);
void __dcc_memprof_flush(void);

void __dcc_memprof_init(void) {
    if (_dcc_memprof_initialized) return;
    _dcc_memprof_initialized = 1;
    atexit(__dcc_memprof_flush);
}

/* 拦截 malloc: 记录 (alloc site, size), 注册 active 分配, 返回指针 */
void *__dcc_memprof_alloc_record(uint64_t size, void *return_addr) {
    __dcc_memprof_init();
    void *p = malloc((size_t)size);
    if (!p) return NULL;
    struct _mp_stack st;
    _mp_stack_capture(&st, return_addr);
    int idx = _mp_find_site(&st);
    if (idx < 0) idx = _mp_new_site(&st);
    if (idx >= 0) {
        _mp_sites[idx].count++;
        _mp_sites[idx].bytes += size;
        for (int i = 0; i < _MP_MAX_ACTIVE; i++) {
            if (!_mp_active[i].in_use) {
                _mp_active[i].ptr = p;
                _mp_active[i].size = size;
                _mp_active[i].site_idx = idx;
                _mp_active[i].in_use = 1;
                break;
            }
        }
    }
    return p;
}

/* 拦截 free: 记录 dealloc site + lifetime, 释放内存 */
void __dcc_memprof_free_record(void *ptr, void *return_addr) {
    __dcc_memprof_init();
    struct _mp_stack st;
    _mp_stack_capture(&st, return_addr);
    int d_idx = _mp_find_dealloc(&st);
    if (d_idx < 0) d_idx = _mp_new_dealloc(&st);
    if (d_idx >= 0) _mp_deallocs[d_idx].count++;

    for (int i = 0; i < _MP_MAX_ACTIVE; i++) {
        if (_mp_active[i].in_use && _mp_active[i].ptr == ptr) {
            int a_idx = _mp_active[i].site_idx;
            uint64_t sz = _mp_active[i].size;
            _mp_active[i].in_use = 0;
            if (a_idx >= 0 && d_idx >= 0) _mp_record_lifetime(a_idx, d_idx, sz);
            break;
        }
    }
    free(ptr);
}

static void _mp_wr32(FILE *f, uint32_t v) {
    unsigned char b[4] = {
        (unsigned char)(v & 0xff),
        (unsigned char)((v >> 8) & 0xff),
        (unsigned char)((v >> 16) & 0xff),
        (unsigned char)((v >> 24) & 0xff),
    };
    fwrite(b, 1, 4, f);
}

static void _mp_wr64(FILE *f, uint64_t v) {
    unsigned char b[8];
    for (int i = 0; i < 8; i++) {
        b[i] = (unsigned char)((v >> (8 * i)) & 0xff);
    }
    fwrite(b, 1, 8, f);
}

static void _mp_wr_str(FILE *f, const char *s) {
    uint32_t len = (uint32_t)strlen(s);
    _mp_wr32(f, len);
    fwrite(s, 1, len, f);
}

static void _mp_wr_stack(FILE *f, const struct _mp_stack *s) {
    char buf[32];
    _mp_wr32(f, (uint32_t)s->n);
    for (int i = 0; i < s->n; i++) {
        snprintf(buf, sizeof buf, "0x%016lx", (unsigned long)s->frames[i]);
        _mp_wr_str(f, buf);
    }
}

static const char *_mp_output_path(void) {
    static char buf[1024];
    const char *out = getenv("DCC_MEMPROF_OUT");
    if (out && *out) {
        snprintf(buf, sizeof buf, "%s", out);
        return buf;
    }
    snprintf(buf, sizeof buf, "memprof.memprofdata");
    return buf;
}

void __dcc_memprof_flush(void) {
    if (!_dcc_memprof_initialized) return;
    FILE *f = fopen(_mp_output_path(), "wb");
    if (!f) return;
    fwrite("MEMPROFD", 1, 8, f);
    _mp_wr32(f, 1); /* version */

    int n_allocs = 0;
    for (int i = 0; i < _MP_MAX_SITES; i++) {
        if (_mp_sites[i].in_use) n_allocs++;
    }
    _mp_wr32(f, (uint32_t)n_allocs);
    for (int i = 0; i < _MP_MAX_SITES; i++) {
        if (!_mp_sites[i].in_use) continue;
        _mp_wr_stack(f, &_mp_sites[i].stack);
        _mp_wr64(f, _mp_sites[i].count);
        _mp_wr64(f, _mp_sites[i].bytes);
    }

    int n_life = 0;
    for (int i = 0; i < _MP_MAX_LIFE; i++) {
        if (_mp_life[i].in_use) n_life++;
    }
    _mp_wr32(f, (uint32_t)n_life);
    for (int i = 0; i < _MP_MAX_LIFE; i++) {
        if (!_mp_life[i].in_use) continue;
        _mp_wr_stack(f, &_mp_sites[_mp_life[i].a_idx].stack);
        _mp_wr_stack(f, &_mp_deallocs[_mp_life[i].d_idx].stack);
        _mp_wr64(f, _mp_life[i].bytes);
    }

    int n_deallocs = 0;
    for (int i = 0; i < _MP_MAX_DEALLOC; i++) {
        if (_mp_deallocs[i].in_use) n_deallocs++;
    }
    _mp_wr32(f, (uint32_t)n_deallocs);
    for (int i = 0; i < _MP_MAX_DEALLOC; i++) {
        if (!_mp_deallocs[i].in_use) continue;
        _mp_wr_stack(f, &_mp_deallocs[i].stack);
        _mp_wr64(f, _mp_deallocs[i].count);
    }
    fclose(f);
    _dcc_memprof_initialized = 0;
}
"#
        .to_string()
    }
}

fn read_u32(data: &[u8], off: &mut usize) -> io::Result<u32> {
    if *off + 4 > data.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated u32"));
    }
    let v = u32::from_le_bytes(data[*off..*off + 4].try_into().unwrap());
    *off += 4;
    Ok(v)
}

fn read_u64(data: &[u8], off: &mut usize) -> io::Result<u64> {
    if *off + 8 > data.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated u64"));
    }
    let v = u64::from_le_bytes(data[*off..*off + 8].try_into().unwrap());
    *off += 8;
    Ok(v)
}

fn read_string(data: &[u8], off: &mut usize) -> io::Result<String> {
    let len = read_u32(data, off)? as usize;
    if *off + len > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated string",
        ));
    }
    let s = std::str::from_utf8(&data[*off..*off + len])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .to_string();
    *off += len;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_memprofdata() {
        let d = MemProfData::from_str(
            "alloc main -> foo -> malloc = 100 4096\nlifetime main -> foo -> malloc => free = 4096\n",
        )
        .unwrap();
        assert_eq!(
            d.alloc_counts
                .get(&vec!["main".into(), "foo".into(), "malloc".into()]),
            Some(&100)
        );
        // 100 < 200 -> is_cold == true
        assert!(d.is_cold(&["main".into(), "foo".into(), "malloc".into()], 200));
        // 100 >= 50 -> is_cold == false
        assert!(!d.is_cold(&["main".into(), "foo".into(), "malloc".into()], 50));
        // 未知 callstack 视为 0 < 任何 threshold -> is_cold == true
        assert!(d.is_cold(&["main".into()], 200));
    }

    #[test]
    fn parse_dealloc() {
        let d = MemProfData::from_str("dealloc free = 50\ndealloc my_free = 30\n").unwrap();
        assert_eq!(d.dealloc_counts.get(&vec!["free".into()]), Some(&50));
        assert_eq!(d.dealloc_counts.get(&vec!["my_free".into()]), Some(&30));
    }

    #[test]
    fn parse_accumulates_duplicates() {
        let d = MemProfData::from_str("alloc main = 10 100\nalloc main = 20 200\n").unwrap();
        assert_eq!(d.alloc_counts.get(&vec!["main".into()]), Some(&30));
        assert_eq!(d.alloc_bytes.get(&vec!["main".into()]), Some(&300));
    }

    #[test]
    fn classify_alloc_sites() {
        let mut d = MemProfData::default();
        d.alloc_counts.insert(vec!["hot_alloc".into()], 200);
        d.alloc_counts.insert(vec!["normal_alloc".into()], 50);
        d.alloc_counts.insert(vec!["cold_alloc".into()], 2);
        assert_eq!(d.classify(&["hot_alloc".into()], 5, 100), AllocClass::Hot);
        assert_eq!(
            d.classify(&["normal_alloc".into()], 5, 100),
            AllocClass::Normal
        );
        assert_eq!(d.classify(&["cold_alloc".into()], 5, 100), AllocClass::Cold);
    }

    #[test]
    fn classify_ephemeral() {
        let mut d = MemProfData::default();
        d.alloc_counts.insert(vec!["fast".into()], 8);
        d.lifetime
            .insert((vec!["fast".into()], vec!["free".into()]), 16);
        assert_eq!(d.classify(&["fast".into()], 5, 100), AllocClass::Ephemeral);
    }

    #[test]
    fn merge_profiles() {
        let mut a = MemProfData::default();
        a.alloc_counts.insert(vec!["f".into()], 10);
        let mut b = MemProfData::default();
        b.alloc_counts.insert(vec!["f".into()], 20);
        b.alloc_counts.insert(vec!["g".into()], 5);
        a.merge(&b);
        assert_eq!(a.alloc_counts.get(&vec!["f".into()]), Some(&30));
        assert_eq!(a.alloc_counts.get(&vec!["g".into()]), Some(&5));
    }

    #[test]
    fn summary_stats() {
        let mut d = MemProfData::default();
        d.alloc_counts.insert(vec!["hot".into()], 200);
        d.alloc_counts.insert(vec!["cold".into()], 1);
        d.alloc_bytes.insert(vec!["hot".into()], 8000);
        d.alloc_bytes.insert(vec!["cold".into()], 64);
        d.lifetime
            .insert((vec!["hot".into()], vec!["free".into()]), 4000);
        d.dealloc_counts.insert(vec!["free".into()], 100);
        let s = d.summary();
        assert_eq!(s.total_allocs, 201);
        assert_eq!(s.total_bytes, 8064);
        assert_eq!(s.unique_alloc_sites, 2);
        assert_eq!(s.unique_dealloc_sites, 1);
        assert_eq!(s.lifetime_entries, 1);
        assert_eq!(s.hot_sites, 1);
        assert_eq!(s.cold_sites, 1);
    }

    #[test]
    fn roundtrip_to_string() {
        let mut d = MemProfData::default();
        d.alloc_counts
            .insert(vec!["main".into(), "malloc".into()], 100);
        d.alloc_bytes
            .insert(vec!["main".into(), "malloc".into()], 4096);
        d.lifetime.insert(
            (vec!["main".into(), "malloc".into()], vec!["free".into()]),
            4096,
        );
        d.dealloc_counts.insert(vec!["free".into()], 50);
        let text = d.serialize();
        let d2 = MemProfData::from_str(&text).unwrap();
        assert_eq!(d2.alloc_counts, d.alloc_counts);
        assert_eq!(d2.alloc_bytes, d.alloc_bytes);
        assert_eq!(d2.lifetime, d.lifetime);
        assert_eq!(d2.dealloc_counts, d.dealloc_counts);
    }

    #[test]
    fn read_from_file() {
        let tmp = std::env::temp_dir().join("dcc-memprof-test-v1.txt");
        std::fs::write(
            &tmp,
            b"alloc main -> foo -> malloc = 100 4096\ndealloc free = 50\n",
        )
        .unwrap();
        let d = MemProfData::read(&tmp).unwrap();
        assert_eq!(
            d.alloc_counts
                .get(&vec!["main".into(), "foo".into(), "malloc".into()]),
            Some(&100)
        );
        assert_eq!(d.dealloc_counts.get(&vec!["free".into()]), Some(&50));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn alloc_sites_and_bytes() {
        let mut d = MemProfData::default();
        d.alloc_counts.insert(vec!["a".into()], 10);
        d.alloc_counts.insert(vec!["b".into()], 20);
        d.alloc_bytes.insert(vec!["a".into()], 100);
        assert_eq!(d.alloc_sites().len(), 2);
        assert_eq!(d.bytes_for_site(&["a".into()]), 100);
        assert_eq!(d.bytes_for_site(&["unknown".into()]), 0);
    }

    #[test]
    fn config_defaults() {
        let c = MemProfConfig::new();
        assert!(!c.instrument);
        assert!(c.use_file.is_none());
        assert_eq!(c.cold_threshold, 5);
    }

    /// Binary format roundtrip.
    #[test]
    fn memprof_binary_roundtrip() {
        let mut d = MemProfData::default();
        d.alloc_counts
            .insert(vec!["main".into(), "malloc".into()], 100);
        d.alloc_bytes
            .insert(vec!["main".into(), "malloc".into()], 4096);
        d.lifetime.insert(
            (vec!["main".into(), "malloc".into()], vec!["free".into()]),
            4096,
        );
        d.dealloc_counts.insert(vec!["free".into()], 50);

        let tmp = std::env::temp_dir().join("dcc-memprof-bin.memprofdata");
        let _ = std::fs::remove_file(&tmp);
        let n = d.write_binary(&tmp).unwrap();
        assert!(n > 12);

        let d2 = MemProfData::read(&tmp).unwrap();
        assert_eq!(d2.alloc_counts, d.alloc_counts);
        assert_eq!(d2.alloc_bytes, d.alloc_bytes);
        assert_eq!(d2.lifetime, d.lifetime);
        assert_eq!(d2.dealloc_counts, d.dealloc_counts);
        let _ = std::fs::remove_file(&tmp);
    }

    /// Binary format rejects bad magic.
    #[test]
    fn memprof_binary_rejects_bad_magic() {
        let result = MemProfData::read_binary(b"BADMAGIC\x01\x00\x00\x00");
        assert!(result.is_err());
    }

    /// read() auto-detects text vs binary.
    #[test]
    fn memprof_read_auto_detects_format() {
        // Text file
        let tmp_txt = std::env::temp_dir().join("dcc-memprof-auto.txt");
        std::fs::write(&tmp_txt, b"alloc main = 10 100\n").unwrap();
        let d1 = MemProfData::read(&tmp_txt).unwrap();
        assert_eq!(d1.alloc_counts.get(&vec!["main".into()]), Some(&10));
        let _ = std::fs::remove_file(&tmp_txt);

        // Binary file
        let tmp_bin = std::env::temp_dir().join("dcc-memprof-auto.bin");
        let mut d = MemProfData::default();
        d.alloc_counts.insert(vec!["test".into()], 5);
        d.alloc_bytes.insert(vec!["test".into()], 200);
        d.write_binary(&tmp_bin).unwrap();
        let d2 = MemProfData::read(&tmp_bin).unwrap();
        assert_eq!(d2.alloc_counts.get(&vec!["test".into()]), Some(&5));
        let _ = std::fs::remove_file(&tmp_bin);
    }

    /// MemProfInstrumentor generates correct call strings.
    #[test]
    fn memprof_instrumentor_calls() {
        let alloc_call = MemProfInstrumentor::alloc_record_call("size", "__ret");
        assert!(alloc_call.contains("__dcc_memprof_alloc_record"));
        assert!(alloc_call.contains("size"));
        let free_call = MemProfInstrumentor::free_record_call("ptr", "__ret");
        assert!(free_call.contains("__dcc_memprof_free_record"));
        assert!(free_call.contains("ptr"));
    }

    /// MemProfInstrumentor lists runtime symbols.
    #[test]
    fn memprof_runtime_symbols() {
        let syms = MemProfInstrumentor::runtime_symbols();
        assert!(syms.contains(&"__dcc_memprof_alloc_record"));
        assert!(syms.contains(&"__dcc_memprof_free_record"));
        assert!(syms.contains(&"__dcc_memprof_init"));
        assert!(syms.contains(&"__dcc_memprof_flush"));
    }

    /// MemProfRuntime generates real C runtime code (no TODO stubs):
    /// callstack capture, active-alloc tracking, lifetime pairing and
    /// .memprofdata binary output must all be present.
    #[test]
    fn memprof_runtime_generates_real_code() {
        let code = MemProfRuntime::generate_runtime();
        assert!(!code.contains("TODO"), "runtime 不应是 stub");
        assert!(code.contains("__dcc_memprof_alloc_record"));
        assert!(code.contains("__dcc_memprof_free_record"));
        assert!(code.contains("__dcc_memprof_init"));
        assert!(code.contains("__dcc_memprof_flush"));
        assert!(code.contains("atexit"));
        // callstack 采集
        assert!(code.contains("__builtin_return_address"));
        assert!(code.contains("_mp_stack_capture"));
        // active 分配表 + lifetime 配对
        assert!(code.contains("_mp_active"));
        assert!(code.contains("_mp_record_lifetime"));
        // .memprofdata 写出
        assert!(code.contains("MEMPROFD"));
        assert!(code.contains("fopen"));
        assert!(code.contains("DCC_MEMPROF_OUT"));
        // 拦截式 ABI: alloc 返回指针, free 内部释放
        assert!(code.contains("void *__dcc_memprof_alloc_record"));
        assert!(code.contains("free(ptr)"));
    }
}
