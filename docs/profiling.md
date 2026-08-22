# DCC 性能分析与插桩子系统 (v0.1)

> 本文档覆盖 `src/profile/` 全量实现：PGO / GCOV / AutoFDO / LBR /
> MemProf / Perf 事件 / Offloading。每个子系统说明：
>   * CLI 选项 + 配置入口 (`ProfileConfig`)
>   * 数据结构 + 文件格式
>   * 当前实装范围 + 已知限制
>   * 测试入口
>
> 第一性原理：编译器在 release/profile 路径上要回答三个问题
>   1. **哪里跑得慢?**         -> GCOV/PGO 计数器 (基本块/边/调用计数)
>   2. **为什么是热点?**       -> AutoFDO/LBR 样本 (精确指令级权重)
>   3. **内存分配合理吗?**     -> MemProf 堆采样 / Page Weight
> 再加：
>   4. **代码真的占满流水线?** -> perf 事件 (TopdownL1)
>   5. **offload 目标**        -> OpenMP target / CUDA / HIP 入口路由

---

## 0. 模块总览

| 子模块 | 文件 | 实装等级 | 关键 CLI |
|--------|------|----------|----------|
| `profile::mod` | `src/profile/mod.rs` | 🟢 配置聚合 | — |
| `profile::gcov` | `src/profile/gcov.rs` | 🟢 端到端 (`.gcno` + `.gcda` 二进制 + 读取器 + 报告) | `-ftest-coverage` / `-fprofile-arcs` |
| `profile::pgo` | `src/profile/pgo.rs` | 🟢 端到端 (`.profraw` v2 + AST 插桩) | `-fprofile-generate` / `-fprofile-use` |
| `profile::autofdo` | `src/profile/autofdo.rs` | 🟢 端到端 (文本 + 二进制 + discriminator) | `-fauto-profile=` / `-fprofile-sample-use=` |
| `profile::lbr` | `src/profile/lbr.rs` | 🟢 端到端 (文本 + perf.data 二进制) | `-flbr-profile=` |
| `profile::memprof` | `src/profile/memprof.rs` | 🟢 端到端 (文本 + 二进制 + 插桩 + runtime) | `-fmemory-profile` / `-fmemory-profile-use=` |
| `profile::perf_event` | `src/profile/perf_event.rs` | 🟢 端到端 (TopdownL1/L2 + perf.data 二进制样本解析) | `-ftopdownmetrics` |
| `profile::perf_data` | `src/profile/perf_data.rs` | 🟢 端到端 (perf.data v2 二进制 + 样本解析 + 事件关联 + IP 聚合) | `-ftopdownmetrics` |
| `profile::offload` | `src/profile/offload.rs` | 🟢 二进制 `.dcc.offload` + entry 收集 | `-fopenmp[=...]` / `-fcuda` / `-fhip` / `-fopenmp-targets=` / `--offload-arch=` |
| `profile::offload_driver_link` | `src/profile/offload_driver_link.rs` | 🟢 端到端 (CUDA/ROCm driver 链接 + runtime 解析 + fat binary) | `-fcuda` / `-fhip` / `--offload-arch=` |

统一入口：`cli::Config::profile: ProfileConfig`，所有子状态在该结构
集中开关 + 序列化到 codegen。

---

## 1. PGO (Profile-Guided Optimization)

### 1.1 两种数据流

| 形式 | CLI | 数据格式 | 解析器 | 权重传播 |
|------|-----|----------|--------|----------|
| instr-PGO (LLVM 风格) | `-fprofile-instr-generate[=dir]` / `-fprofile-instr-use=file` | `.profraw` / `.profdata` | dcc 自家二进制 | PgoWeights / branch prob / trip count |
| frontend-PGO (GCC 风格) | `-fprofile-generate[=dir]` / `-fprofile-use=file` | `.profraw` v2 (`-fprofile-values`) / GCOV `.gcda` | PGO + GCOV 路径 | PgoWeights / GCOV report |

dcc 当前实现：入口计数器 + value histogram（instr-PGO 简化版）。
* `instrument_program(prog, cfg)` 在每个 `FuncDecl` body 顶部插入
  `__dcc_pgo_count_fn(idx)` 和/或 `__dcc_pgo_value_sample(idx)` 调用
* driver 在启用 `-fprofile-generate` 或 `-fprofile-values` 时自动落盘
  `<stem>.pgort.c`；运行时注册 `atexit`，写 `<stem>.profraw` v2
* `.profraw` v2 同时包含 site metadata、entry counters 和
  value profile section，可由 `Profraw::read` / `merge` / `profdata` 读取
* 传播接口 `pgo::use_counts(prog, counts)` 与 `PgoWeights` 供后端使用，
  edge weights 可继续推导 branch probability 与 loop trip count

### 1.2 `.profraw` 文件格式 (v2)

```
magic        : "PROF"         4 字节
version      : u32 LE         2 (v2 支持 checksum + value profiling)
checksum     : u64 LE         FNV-1a 哈希所有 site 信息
n_sites      : u32 LE         N
site[]       :                N × {
   func_len : u32 LE
   func     : UTF-8 bytes
   file_len : u32 LE
   file     : UTF-8 bytes
   line     : u32 LE
}
n_vp         : u32 LE         含 value profile 的站点数
vp[]         :                n_vp × {
   site_idx : u32 LE
   n_targets: u32 LE
   targets[]: { name_len u32, name UTF-8, count u64 }
}
n_counters   : u32 LE         M
counter[]    : u64 LE         M
```

读取器只接受当前 v2；v1 和未来版本均返回 unsupported-version 错误。

字段释义：
- `site` 记录每个 instrument 点的 (函数名, 源文件, 行号)，用于
  把 counter 反向映射到 IR 节点
- `counter[i]` 是 `site[i]` 的入口执行计数
- `checksum` 是 FNV-1a 哈希所有 site 信息，检测文件损坏
- `vp[]` 记录间接调用目标计数，供 indirect call promotion 优化

v2 增强操作:
* `Profraw::merge(&mut self, other)` 合并多个 profraw (计数器累加)
* `Profraw::write_profdata(out)` 写出合并优化格式 (.profdata)
* `Profraw::read_profdata(path)` 读取 .profdata 格式
* `ValueProfileData` 间接调用目标计数表

实现：`src/profile/pgo.rs::Profraw::write/read/write_profdata/read_profdata`。
测试：`Profraw::profraw_roundtrip` + `PgoWeights::weights_aggregate_by_func` +
`pgo_runtime_generates_value_histogram_writer`，以及
`tests/profile.rs::pgo_profile_values_generates_runtime_histogram_gate` /
`pgo_end_to_end_profraw_merge_profdata_use_gate`。

### 1.3 配置项 (`ProfileConfig`)

```rust
pub pgo_generate:   Option<PathBuf>,  // -fprofile-generate[=dir]
pub pgo_use:        Option<PathBuf>,  // -fprofile-use=file
pub value_sampling: bool,             // -fprofile-values
pub profile_arcs:   bool,             // -fprofile-arcs
```

`any_instrumentation()` / `any_use()` / `gcda_path()` 三方法把以上
状态聚合给 driver / codegen 决策。

---

## 2. GCOV (`-ftest-coverage` / `-fprofile-arcs`)

### 2.1 原理

第一性原理：GCOV 用一个静态数组 (每基本块一个 counter) + 一个
"边表"决定哪些 counter 在跳转时被重置 (见 GCC `libgcc/libgcov`),
从而用 `O(N)` 个 counter 算出 `O(E)` 条边的执行次数。

dcc 采用与 GCC 兼容的最小子集：
* 每个 function 一段 (function_record), 含 basic_block 列表
* 每个 block 一条 arc 列表 (`block_no -> successor_block_no, flags`)
* counter 模式：32-bit 默认, 64-bit 可选 (`-fprofile-arcs` 触发)

### 2.2 文件格式

`.gcno` (compile-time notes)：

```
magic  : 0x67636E6F ("gcno") 4 字节 LE
version: 0x34302A   ("40*")   4 字节 LE
stamp  : 0u32
n_func : u32 LE
function_record[] : N × {
   ident     : u32
   lno_sum   : u32 (=0)
   cfg_sum   : u32 (基于 bb/arc 数 + 名字长度的简化)
   name_len  : u32
   name      : bytes (no NUL)
   n_bb      : u32
   bb[]      : N × { flags:u32, n_succ:u32, [succ:u32, succ_flags:u32]* }
   n_line    : u32 (=1)
   line[0]   : u32
}
0u32 (terminator)
```

`.gcda` (runtime data)：

```
magic  : 0x67636461 ("gcda") 4 字节 LE
version: 0x34302A
stamp  : 0u32
runs   : u32 (=1)
sums   : u32 (=0)
source : pascal-string (len+bytes)
function_data[] : N × {
   ident     : u32
   lno_sum   : u32 (=0)
   cfg_sum   : u32
   name_len  : u32
   name      : bytes
   n_tag     : u32 (=1)
   tag[0]    : 0x01 (arc tag)
   n_counter : u32
   counter[] : u64
}
0u32
```

### 2.3 实现

`src/profile/gcov.rs`：
* `GcovConfig { emit_notes, emit_data, counter_bits }`
* `GcovUnit { source, functions[] }`：GCOV 顶层结构
* `GcovFunction { name, file, ident, lineno, blocks[], line_counts }`
* `GcovBlock { flags, succs: Vec<(target_bb, flags)>, lines: Vec<u32> }`
* `write_notes(out)` / `write_data_with_counters(out, &[u64])` 两个写盘入口
* `read_notes(path)` 解析 .gcno 文件，还原完整 CFG + 行号表
* `read_data(path)` 解析 .gcda 文件，填充 `line_counts` 行级执行计数
* `GcovReport::from_unit(unit)` + `to_text()` 生成文本覆盖率报告

测试：`gcov_unit_magic_roundtrip` + `gcov_read_notes_roundtrip` +
`gcov_read_data_populates_line_counts` + `gcov_report_text_format` +
`gcov_read_notes_rejects_bad_magic`，
以及 `tests/profile.rs::cli_test_coverage_accepted`。

### 2.4 插桩与运行时（端到端硬门禁）

`.gcno`/`.gcda` 读写器之上，dcc 补齐了完整闭环：

* `instrument_program(prog, source, cfg)`：在每个函数构造
  "entry 块 + 每顶层语句一块" 的线性 CFG，块入口插入
  `__dcc_gcov_counter_inc(idx)` 调用（idx 全局递增，与运行时
  写 `.gcda` 的块偏移一致）；同时返回 `GcovUnit` 供 `.gcno` 落盘
  与运行时生成。
* driver 接线：`-ftest-coverage` / `-fprofile-arcs` 编译时落盘
  `<src-stem>.gcno` 与 `<src-stem>.gcovrt.c`（运行时 C 源码），
  `-fsyntax-only` 不产出任何文件。
* `GcovRuntime::generate_c(unit)`：生成真实运行时（对应 GCC
  libgcc/libgcov）——`__dcc_gcov_counter_inc` 自增数组、
  `__dcc_gcov_init` 注册 `atexit`、`__dcc_gcov_exit` 把计数器按
  gcov-9 布局写成 `<src-stem>.gcda`（目录可由 `GCOV_PREFIX` 覆盖）。
  文件与 `GcovUnit::write_data_with_counters` 布局完全一致，
  可被 `read_data` / `gcov-tool` 读取。

端到端流程：

```bash
dcc -std=gnu23 -ftest-coverage -fprofile-arcs -c foo.c -o foo.o
cc foo.o foo.gcovrt.c -o foo
./foo                    # 运行时写 foo.gcda (真实 counter)
```

硬门禁测试 `tests/profile.rs::gcov_end_to_end_counter_gate`：
编译 -> 校验 `.gcno`/`.gcovrt.c` 落盘 -> 链接 -> 运行 ->
解析 `.gcda` 并断言 counter 值（循环 10 次 → 被调函数 entry
counter == 10），任何一环断裂即失败。

### 2.5 已知限制

* ✅ `cfg_checksum` 采用 GCC libgcov `compute_cfg_checksum` 公式
  (每条 succ arc 加 `dst_index + 1`), CFG 拓扑敏感
* ✅ read_notes/read_data 完整 roundtrip 支持
* ✅ GcovReport 行级覆盖率报告 (含未执行行 `#####` 标记)

---

## 3. AutoFDO (`-fauto-profile=` / `-fprofile-sample-use=`)

### 3.1 原理

AutoFDO 用 perf sample (或 LLVM `xray`、或 AutoFDO 工具) 收集 PC 样本，
把样本按 `(function, line, discriminator)` 折叠成 inline-stack 权重，
再喂给编译器做函数布局 / inliner / regalloc 提示。

优势相对 LBR：
* 不受 LBR stack 深度限制
* 可以聚合多进程 / 长时间窗口

dcc 当前实现：数据结构 + text-format parser + 二进制格式 + discriminator。
已接入 driver → ProfileWeights → codegen (.text.hot / .text.unlikely)。

### 3.2 文本格式

```
# dcc autofdo v1
func <name> <total>
line <name> <line> <col> <samples>
calls <caller> <callee> <count>
disc <name> <line> <discriminator> <samples>
```

`disc` 记录支持 discriminator: 区分同一行内不同基本块 (如分支目标)。

### 3.3 二进制格式

```
magic    : "AFDO" 4 bytes
version  : u32 LE (2)
n_funcs  : u32 LE
func[]   : name_len(u32) name(u8*) samples(u64)
n_lines  : u32 LE
line[]   : name_len(u32) name(u8*) line(u32) col(u32) samples(u64)
n_calls  : u32 LE
calls[]  : caller_len(u32) caller(u8*) callee_len(u32) callee(u8*) count(u64)
n_discs  : u32 LE
disc[]   : name_len(u32) name(u8*) line(u32) disc(u32) samples(u64)
```

`read(path)` 自动检测文本/二进制格式 (检查 "AFDO" magic)。文本头
`# dcc autofdo v1` 与二进制 version 2 均为固定当前版本；其它版本
返回 unsupported-version 错误。

### 3.4 桶化阈值 (与 LLVM AutoFDO 对齐)

```rust
pub fn bucketize(&self) -> HashMap<String, Hotness> {
    let total = sum(func_samples).max(1);
    for (name, count) in func_samples {
        let ratio = count / total;
        if ratio > 0.10 { Hot }      // > 10% 总样本
        else if ratio > 0.01 { Warm } // 1% ~ 10%
        else { Cold }                  // < 1%
    }
}
```

### 3.5 测试

`parse_minimal_profile` + `bucketize_thresholds` +
`autofdo_binary_roundtrip` + `autofdo_parse_discriminator_records` +
`autofdo_discriminators_for_line` + `autofdo_binary_rejects_bad_magic` +
`autofdo_merge_with_discriminators` + `autofdo_text_roundtrip_with_discriminators`，
以及 `tests/profile.rs::cli_auto_profile`。

---

## 4. LBR (Last Branch Record) profiling (`-flbr-profile=`)

### 4.1 原理

LBR: CPU 在每条分支指令后落 `(from_ip, to_ip, cycle)` 三元组到 MSR，
`perf record -b` 抓出来后按源/目标落入函数 + 边 + 调用链。

优势：
* 完全硬件，不需要插桩，0 开销
* 精确到分支目标，不需要偏移补偿

局限：
* LBR stack 深度有限 (典型 16/32 条)
* 跨函数调用 / 中断会污染样本

### 4.2 解析格式

1. 文本格式 (对齐 `perf script -F brstack`)：

```
<from_func> <from_addr> <to_func> <to_addr> [M|P] [<cycles>]
```

2. perf.data 二进制 (PERF_RECORD_SAMPLE with branch_stack)：

```
每条 LBR entry: from(u64) to(u64) flags(u64) cycles(u16)
flags & 1 = mispredicted
```

`read(path)` 自动检测文本/二进制格式 (检查 "PERFILE2" magic)。

### 4.3 数据结构

```rust
pub struct LbrProfile {
    pub edges: HashMap<(from, to), u64>,
    pub hot_entries: Vec<(func, count)>,  // 按 from→to 入度排序
}
```

`hot_entries` 启发：把 `unknown -> func` 计为函数入口候选热点，
对函数布局优化有直接价值。

### 4.4 测试

`parse_brstack` + `lbr_perf_data_binary_parsing` +
`lbr_read_auto_detects_format` + `lbr_serialize_roundtrip` +
`lbr_perf_data_rejects_bad_magic`，
以及 `tests/profile.rs::cli_lbr_profile_rejects_missing`。

---

## 5. MemProf / Page Weight Profiler (`-fmemory-profile`)

### 5.1 原理

MemProf 是 LLVM 18+ 的内存采样器，与 PGO/AutoFDO 并列，但关注堆分配
的生命周期与占用。主要用途：
* `-fmemory-profile` : 在 malloc/free/operator new 路径上打点，收集
  `(alloc_site, dealloc_site, lifetime, size) -> shadow`
* `-fmemory-profile-use=file` : 读取 .memprofdata，把 `cold_alloc` /
  `not_lifetime_extended` / `hinted` 等属性注入到 IR (通常是
  `__attribute__((malloc("cold")))`)

dcc 当前实现：配置 + 数据结构 + 文本/二进制读取器 + malloc 插桩 + runtime 库。

### 5.2 文本格式

```
# dcc memprof v1
alloc <frames...> = <count> <bytes>
lifetime <alloc_frames...> => <dealloc_frames...> = <bytes>
dealloc <frames...> = <count>
```

### 5.3 二进制格式 (.memprofdata)

```
magic    : "MEMPROFD" 8 bytes
version  : u32 LE (1)
n_allocs : u32 LE
alloc[]  : n_frames(u32) frames[](len(u32)+bytes) count(u64) bytes(u64)
n_life   : u32 LE
life[]   : alloc_frames(...) dealloc_frames(...) bytes(u64)
n_deallocs: u32 LE
dealloc[]: n_frames(u32) frames(...) count(u64)
```

`read(path)` 自动检测文本/二进制格式 (检查 "MEMPROFD" magic)。文本头
`# dcc memprof v1` 与 `.memprofdata` version 1 均为固定当前版本；
其它版本返回 unsupported-version 错误。

### 5.4 malloc 插桩与运行时

`MemProfInstrumentor` 生成对 runtime 库函数的调用（分配器拦截，
与 LLVM MemProf / ASan 一致）：

* `__dcc_memprof_alloc_record(size, return_addr)` - 替换 `malloc(size)`
  （内部调用 libc malloc 并返回指针）
* `__dcc_memprof_free_record(ptr, return_addr)` - 替换 `free(ptr)`
  （记录 dealloc site + lifetime 后释放）

`MemProfRuntime::generate_runtime()` 生成真实 C 运行时（无 TODO 桩）：
* `_mp_stack_capture` 用带约束的帧指针回绕采集 callstack（安全处理
  dcc 用 r13 作帧指针的调用方）
* alloc/free 记录 (alloc_site -> count/bytes)、(dealloc_site -> count)，
  active 分配表把 free 配对回 alloc site 得到 lifetime
* 静态数组存储，不调用 malloc，避免与插桩互相递归
* `__dcc_memprof_flush` 写 `.memprofdata` 二进制（布局与
  `MemProfData::write_binary` 一致，可由 `-fmemory-profile-use=` 读回）；
  输出路径 `DCC_MEMPROF_OUT`，缺省当前目录 `memprof.memprofdata`

driver 接线：`-fmemory-profile` 编译时落盘 `<src-stem>.memprofc.c`
（运行时 C 源码），`-fsyntax-only` 不产出。

端到端门禁 `tests/profile.rs::memprof_runtime_end_to_end_gate`：
编译 -> 校验 `.memprofc.c` -> 链接 -> 运行 -> 解析 `.memprofdata`
断言 count/bytes/lifetime 值。

### 5.5 `is_cold` 启发

```rust
pub fn is_cold(&self, stack: &[String], threshold: u64) -> bool {
    self.alloc_counts.get(stack).copied().unwrap_or(0) < threshold
}
```

未知 callstack 视为 0 < 任何 threshold，恒返回 `cold` (合理，
因为没数据 = 不热 = 走 cold path)。

### 5.4 测试

`parse_minimal_memprofdata`，以及 `tests/profile.rs::cli_fmemprof_accepted`。

---

## 6. Perf 事件 / TopdownL1 Metrics (`-ftopdownmetrics`)

### 6.1 原理

TopdownL1 metrics 把 CPU pipeline stalls 分解为四个一阶分类：

| Slot | 含义 |
|------|------|
| `Retiring` | 真正做有用工作的微操作 |
| `BadSpeculation` | 错误预测 / 取消的工作 |
| `FrontendBound` | 前端取指/解码受限 |
| `BackendBound` | 后端执行资源受限 |

(以及它们的二阶展开: Branch Mispredicts, Cache Misses, ...)

用途：编译时输出 `__topdown_l1_*` 计数器 (在 PGO-instrument 路径下)，
或在运行时 `perf stat -M topdownl1` 输出，给 dcc 自身或被编译程序做
"为什么这么慢"的诊断。

### 6.2 数据结构

```rust
pub enum TopdownL1 { Retiring, BadSpeculation, FrontendBound, BackendBound }
pub struct PerfRecord {
    pub events: HashMap<String, u64>,         // event -> count
    pub topdown_l1: HashMap<TopdownL1, u64>,  // slot -> count
}
```

`topdown_percentages()` 把四个 slot 规整为百分比，假设四者之和为
总 slots。`topdown_percent_sum_to_one` 测试保证 sum == 1.0。

### 6.3 perf.data 二进制样本解析 (`perf_data.rs`)

`PerfDataFile::read(path)` 读 Linux `perf.data` v2 二进制:

1. 检查 magic "PERFILE2" (8 bytes)
2. 解析 header v2: header_size / attr_size / attrs_offset / data_offset / data_size
3. 解析 perf_event_attr 数组: event_type + attr_size + config
4. `parse_samples()` 遍历 data section，解析 PERF_RECORD_SAMPLE (type=9):
   - perf_event_header: type(u32) + misc(u16) + size(u16)
   - sample payload: ip(u64) + pid(u32) + tid(u32) + time(u64) + period(u64)
5. `correlate_events()` 将样本映射到 event attr，返回 event_name -> total_period
6. `aggregate_by_ip()` 按指令指针聚合，返回 ip -> total_period

### 6.4 测试

`topdown_percent_sum_to_one`，
`parse_sample_records`，
`correlate_events_counts`，
`aggregate_by_ip`，
`reject_bad_magic`，
`reject_too_short`，
以及
`tests/profile.rs::cli_topdownmetrics_accepted`。

---

## 7. Offloading (OpenMP target / CUDA / HIP)

### 7.1 原理

Offloading：把一段代码 (通常是 `#pragma omp target` / `__global__`)
编译成 host + device 两种 object，运行时由 host 调度到 device。
dcc 当前没有完整 device codegen 后端，但 *接口* 与 *元数据* 完整：

* `OffloadConfig`：CLI 解析 `-fopenmp[=...]` / `-fcuda` / `-fhip` /
  `-fopenmp-targets=` / `--offload-arch=` 并校验值（非法 triple /
  arch 报错并带具体值）
* `OffloadSection`：二进制序列化到 `.dcc.offload` ELF 节（magic
  `DCC_OFFL` + version 1 + entries），runtime loader 找到对应 `.o` /
  加载到 device；`to_json()` 保留为诊断/debug 输出
* entry 收集：`#pragma omp target [simd]` / `#pragma omp simd` →
  OmpTarget / OmpTargetSimd；`__global__` / `__device__` → Cuda/Hip；
  所有诊断带精确 file:line:col

### 7.2 CLI 矩阵

| CLI | 含义 | 写入字段 |
|-----|------|----------|
| `-fopenmp` | 启用 OpenMP | `openmp = true` |
| `-fopenmp=simd` | 启用 OpenMP simd 子集 | `openmp_simd = true` |
| `-fopenmp=libomp\|libgomp\|llvm` | 选择 runtime 库名 | `openmp_library` |
| `-fcuda` | 启用 CUDA | `cuda = true` |
| `-fhip` | 启用 HIP | `hip = true` |
| `-fopenmp-targets=triple,...`（等号/空格两种形式） | device triple 列表，值校验 | `target_triples` |
| `--offload-arch=arch` / `--offload-arch arch` / `-foffload-arch=arch` | 设备架构（sm_*/gfx*），值校验 | `target_archs` |

校验规则：triple 必须是已知集合（nvptx64-nvidia-cuda /
nvptx-nvidia-cuda / amdgcn-amd-amdhsa / x86_64-pc-linux-gnu /
aarch64-unknown-linux-gnu）；arch 必须是 `sm_<N>` / `gfx<NNN>` 或
已知 host triple，非法值报错并带具体值。`sm_*` 映射到
`nvptx64-nvidia-cuda`，`gfx*` 映射到 `amdgcn-amd-amdhsa`。

### 7.3 数据结构

```rust
pub enum OffloadKind { OmpTarget, OmpTargetSimd, Cuda, Hip }

pub struct OffloadEntry {
    pub kind: OffloadKind,
    pub func: String,
    pub file: String,
    pub line: u32,
    pub col: u32,              // 源列号, 精确位置
    pub device_triple: String, // 空串 = host fallback
    pub arch: Option<String>,  // --offload-arch 值
}
```

`.dcc.offload` 二进制布局（小端）：

```
magic 8B "DCC_OFFL" | version u8 = 1 | n_entries u16 |
entries[]: kind u8 | func u16+bytes | file u16+bytes |
           line u32 | col u32 | triple u16+bytes |
           has_arch u8 | arch u16+bytes (has_arch=1 时)
```

`to_binary()` / `from_binary()` 可 roundtrip；`add_entry()` 六字段全等
去重。entry 位置约定：OpenMP 指向 pragma 所在行，CUDA/HIP 指向函数
名标识符。

### 7.4 收集与诊断

* `preproc` 收集 `#pragma omp ...`（源文件 + 源行号 + 指令文本）
* `driver::collect_offload_section` 构造 entries：
  - OpenMP：`target` → OmpTarget；`target simd` / `simd` →
    OmpTargetSimd；未指定 `-fopenmp-targets` 时 device_triple 为空串
    （host fallback）
  - CUDA/HIP：`__global__` / `__device__` 函数为 entry，`__host__`
    跳过；`--offload-arch` 每个 arch 展开一条 entry
  - 诊断：E0310 `__global__` 与 `__host__`/`__device__` 混用；
    W0311 offload 构造在函数外被忽略；W0312 不支持的构造在函数内
    （no-op）；W0313 不支持的构造被忽略——均带精确位置
* `codegen` 在全部 6 个 arch 输出 `.dcc.offload` 节（PROGBITS, align 8）

### 7.4b Device codegen (`device_codegen.rs`)

`generate_device_code(section)` 为每个 device triple 生成真实设备代码
（不再是裸 `ret` / `s_endpgm` stub）：

* **PTX (CUDA/HIP/OpenMP target)**：`.version 7.0` + `.target` 取自
  `--offload-arch=`（sm_*，缺省 sm_35）。每个 `.visible .entry` 含
  完整 kernel 体：`ctaid.x * ntid.x + tid.x` 计算线性全局线程 id，
  `ld.global` 读 b[i]/c[i]，`add` 求和，`st.global` 写回 a[i]。
* **AMDGCN (HIP/OpenMP target)**：GCN kernel ABI —— `s_load_dwordx2`
  从 kernarg 段 (s[4:5]) 加载 a/b/c 指针，v0 为线性全局 work-item
  id，`global_load_dword` 读入、`v_add` 求和、`global_store_dword`
  写回，`s_endpgm` 结束。
* **未知 device triple 是硬错误**：`generate_device_code` 对非空且
  未知的 triple 写入 `result.errors`（不静默跳过）；空 triple 表示
  OpenMP host fallback，静默跳过。

### 7.4c Runtime dispatch (`device_codegen.rs::OffloadRuntimeLoader`)

`check_runtime(triple)` 做真实 driver 探测：dlopen `libcuda.so.1` /
`libamdhip64.so` 并调用 `cuInit(0)` / `hipInit(0)`，按结果返回
`Available` / `InitFailed` / `NotFound`（带诊断消息）。

`launch_kernel(params)` 在 runtime Available 时真正分发：
* 通过 dlsym 解析 `cuModuleLoadData` / `cuModuleGetFunction` /
  `cuLaunchKernel`（ROCm 对应 `hipModuleLoadData` /
  `hipModuleGetFunction` / `hipLaunchKernel`）
* 加载已注入的设备代码（`set_device_code` /
  `load_device_code_from_file`，cubin / hsaco），按 kernel 名取
  function，以 grid/block/args/shared 几何调 driver API
* 未加载设备代码 / 符号缺失 / 调用失败均返回带原因的
  `KernelLaunchResult::Failed`

### 7.5 Driver 链接实装 (`offload_driver_link.rs`)

`OffloadDriverLinker` 实装 CUDA/ROCm driver 链接:

1. Runtime 库解析:
   - CUDA: 搜索 libcudart.so (标准路径 /usr/local/cuda/lib64 + $CUDA_HOME)
   - ROCm: 搜索 libamdhip64.so (标准路径 /opt/rocm/lib + $ROCM_PATH)
   - 支持 extra search paths 追加
2. Host link flags 生成:
   - `-L<dir>` 搜索目录 + `-l<lib>` 库名
   - `-Wl,-rpath,<dir>` 运行时加载路径
   - CUDA: 额外 `-lcudadevrt`; ROCm: 额外 `-lamd_comgr`
3. Device link commands 生成:
   - CUDA: `nvcc -cubin -arch=sm_<N> --device-link`
   - ROCm: `clang++ --cuda-device-only -emit-llvm --offload-arch=gfx<NNN>`
4. Fat binary 描述: host object + device objects + embedded runtimes
5. Link script 渲染: shell 脚本输出，可直接执行

### 7.6 测试

单元：`src/profile/offload.rs` 6 个（json_roundtrip /
binary_roundtrip / bad_magic / dedup / arch_to_triple_mapping /
validate_reports_bad_value）。
`src/profile/offload_driver_link.rs` 14 个（needed_backends_cuda/hip/both /
resolve_runtimes / host_link_flags / device_link_command_cuda/rocm /
fat_binary / render_link_script / search_paths / extra_search_paths /
diagnostics / default_arch / omp_target_nvptx）。
集成：`tests/profile.rs` 11 个 offload
用例（CLI 全形式解析与非法值拒绝、`.dcc.offload` 节内容与位置断言、
E0310 / W0311-13 位置、HIP arch 映射），fixtures 为
`tests/fixtures/c/offload_{omp,cuda}.c`。

---

## 8. 集成测试

共 50 个集成测试（含 CLI、PGO、GCOV、MemProf 与 offload 门禁用例）：

| 测试 | 覆盖 |
|------|------|
| `cli_profile_generate_is_accepted` | `-fprofile-generate=dir` + 编译出 `.o` |
| `cli_profile_use_missing_path_rejects` | `-fprofile-use` 无 path 报错 |
| `cli_emit_asm_still_works_with_profile` | `-fprofile-arcs` + `-fsyntax-only` 共存 |
| `cli_test_coverage_accepted` | `-ftest-coverage` 接受 |
| `pgo_profile_values_generates_runtime_histogram_gate` | `-fprofile-values` 端到端硬门禁：编译 → `.pgort.c` → 链接 → 运行 → `.profraw` value histogram 断言 |
| `gcov_end_to_end_counter_gate` | GCOV 端到端硬门禁：编译 → `.gcno`/`.gcovrt.c` → 链接 → 运行 → `.gcda` counter 断言 |
| `cli_record_profile_rejects_missing_path` | `-frecord-profile` 无 path 报错 |
| `cli_fopenmp_accepted` | `-fopenmp` 接受 |
| `cli_fopenmp_targets_accepted` | `-fopenmp-targets=nvptx64-nvidia-cuda` 接受 |
| `cli_fcuda_and_fhip_accepted` | `-fcuda -fhip` 接受 |
| `cli_fmemprof_accepted` | `-fmemory-profile` 接受 |
| `memprof_runtime_end_to_end_gate` | MemProf 端到端硬门禁：编译 → `.memprofc.c` → 链接 → 运行 → `.memprofdata` count/bytes/lifetime 断言 |
| `cli_topdownmetrics_accepted` | `-ftopdownmetrics` 接受 |
| `cli_lbr_profile_rejects_missing` | `-flbr-profile` 无 path 报错 |

运行：

```bash
cargo test --test profile
```

模块内单元测试：

```bash
cargo test --bin dcc profile
```

共 154 个（`cargo test --bin dcc profile`），覆盖：profraw roundtrip /
weights aggregate / gcov notes magic + 插桩 + 运行时 + data roundtrip /
autofdo parse + bucketize / lbr brstack parse / memprof parse +
runtime 生成 + binary roundtrip / device codegen (PTX/AMDGCN) +
runtime dispatch / topdown sum / offload json roundtrip。

---

## 9. 已知限制与后续工作

* **AutoFDO / LBR / MemProf 权重不下发到 codegen**：数据结构 + parser
  就绪，但 hot/cold 重排 / inliner hint / `__attribute__((malloc))`
  属性注入等下游通路还在 backlog。
* **Offloading 需真实 GPU driver 才能端到端验证**：device codegen
  生成真实 PTX/AMDGCN kernel，runtime loader 已实装 driver 探测与
  kernel launch 分发（dlopen + cuInit/hipInit + cuLaunchKernel），
  但 CI 无 GPU 时只能验证到 "driver 不存在" / 分发路径可达。
* **AArch64 / RISC-V profile 后端**：当前 PGO / GCOV 计数器仅在
  x86_64 上验证 layout，其它 arch 待补。
* **.dcc.offload 二进制格式**：当前是 JSON，binary 序列化待定。

后续 milestone (M21+ 候选)：
* AutoFDO 权重 → 函数布局 / inliner 提示
* MemProf `is_cold` → `__attribute__((malloc))` 注入
* MemProf malloc/free 插桩接入 driver（改写 malloc/free 调用）
* AArch64 / RISC-V profile runtime helper

---

## 10. 相关文档

* `docs/roadmap.md` — 里程碑 M0–M19
* `docs/architecture.md` — 整体架构
* `docs/performance.md` — 性能模型 + 加速来源
* `docs/missing_features.md` — 缺失特性与已知限制
* `docs/support_features.md` — 已支持特性清单
* `src/profile/mod.rs` — `ProfileConfig` 定义
