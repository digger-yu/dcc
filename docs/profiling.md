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
| `profile::gcov` | `src/profile/gcov.rs` | 🟢 端到端 (`.gcno` + `.gcda` 二进制) | `-ftest-coverage` / `-fprofile-arcs` |
| `profile::pgo` | `src/profile/pgo.rs` | 🟢 端到端 (`.profraw` + AST 插桩) | `-fprofile-generate` / `-fprofile-use` |
| `profile::autofdo` | `src/profile/autofdo.rs` | 🟡 数据结构 + 极简 parser | `-fauto-profile=` / `-fprofile-sample-use=` |
| `profile::lbr` | `src/profile/lbr.rs` | 🟡 解析 `perf script -F brstack` | `-flbr-profile=` |
| `profile::memprof` | `src/profile/memprof.rs` | 🟡 配置 + 数据结构 | `-fmemory-profile` / `-fmemory-profile-use=` |
| `profile::perf_event` | `src/profile/perf_event.rs` | 🟡 TopdownL1 数据结构 | `-ftopdownmetrics` |
| `profile::offload` | `src/profile/offload.rs` | 🟢 二进制 `.dcc.offload` + entry 收集 | `-fopenmp[=...]` / `-fcuda` / `-fhip` / `-fopenmp-targets=` / `--offload-arch=` |

统一入口：`cli::Config::profile: ProfileConfig`，所有子状态在该结构
集中开关 + 序列化到 codegen。

---

## 1. PGO (Profile-Guided Optimization)

### 1.1 两种数据流

| 形式 | CLI | 数据格式 | 解析器 | 权重传播 |
|------|-----|----------|--------|----------|
| instr-PGO (LLVM 风格) | `-fprofile-instr-generate[=dir]` / `-fprofile-instr-use=file` | `.profraw` / `.profdata` | dcc 自家二进制 | stub |
| frontend-PGO (GCC 风格) | `-fprofile-generate[=dir]` / `-fprofile-use=file` | `.gcda` | GCOV 路径 (见 §2) | stub |

dcc 当前实现：入口计数器 (instr-PGO 简化版)。
* `instrument_program(prog, cfg)` 在每个 `FuncDecl` body 顶部插入
  `__dcc_pgo_count_fn(idx)` 调用
* `_dcc_pgo_runtime` 累加 `idx -> counter`，`atexit` 写 `<name>.profraw`
* 读回 (`Profraw::read`) 仅解析 `u64` 数组，不做权重传播
* 传播接口 `pgo::use_counts(prog, counts)` 留给后端，当前仅落
  `PgoWeights: HashMap<func_name, u64>` 备查

### 1.2 `.profraw` 文件格式

```
magic        : "PROF"         4 字节
version      : u32 LE         1
n_sites      : u32 LE         N
site[]       :                N × {
   func_len : u32 LE
   func     : UTF-8 bytes
   file_len : u32 LE
   file     : UTF-8 bytes
   line     : u32 LE
}
n_counters   : u32 LE         M
counter[]    : u64 LE         M
```

字段释义：
- `site` 记录每个 instrument 点的 (函数名, 源文件, 行号)，用于
  把 counter 反向映射到 IR 节点
- `counter[i]` 是 `site[i]` 的入口执行计数

实现：`src/profile/pgo.rs::Profraw::write/read`。
测试：`Profraw::profraw_roundtrip` + `PgoWeights::weights_aggregate_by_func`，
以及 `tests/profile.rs::cli_profile_generate_is_accepted`。

### 1.3 配置项 (`ProfileConfig`)

```rust
pub pgo_generate:  Option<PathBuf>,  // -fprofile-generate[=dir]
pub pgo_use:       Option<PathBuf>,  // -fprofile-use=file
pub profile_arcs:  bool,             // -fprofile-arcs
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
* `GcovFunction { name, file, ident, lineno, blocks[] }`
* `GcovBlock { flags, succs: Vec<(target_bb, flags)> }`
* `write_notes(out)` / `write_data_with_counters(out, &[u64])` 两个写盘入口

测试：`gcov_unit_magic_roundtrip` + `gcov_unit_data_roundtrip`，
以及 `tests/profile.rs::cli_test_coverage_accepted`。

### 2.4 已知限制

* 配合 `llvm-cov gcov` 不可读 (BB 边界 tag 不全)
* ✅ (M24) `cfg_checksum` 采用 GCC libgcov `compute_cfg_checksum` 公式
  (每条 succ arc 加 `dst_index + 1`), CFG 拓扑敏感
* 真实场景下用 `llvm-cov convert-for-llvm` 二次转换

---

## 3. AutoFDO (`-fauto-profile=` / `-fprofile-sample-use=`)

### 3.1 原理

AutoFDO 用 perf sample (或 LLVM `xray`、或 AutoFDO 工具) 收集 PC 样本，
把样本按 `(function, line, discriminator)` 折叠成 inline-stack 权重，
再喂给编译器做函数布局 / inliner / regalloc 提示。

优势相对 LBR：
* 不受 LBR stack 深度限制
* 可以聚合多进程 / 长时间窗口

dcc 当前仅定义数据结构 + 极简 text-format parser，权重不下发到 codegen。

### 3.2 简化格式

```
# header
func <name> <total>
line <name> <line> <col> <samples>
```

### 3.3 桶化阈值 (与 LLVM AutoFDO 对齐)

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

### 3.4 测试

`parse_minimal_profile` + `bucketize_thresholds`，以及
`tests/profile.rs::cli_auto_profile`（待加）。

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

对齐 `perf script -F brstack`：

```
<from_func> <from_addr> <to_func> <to_addr> <cycles>
```

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

`parse_brstack`，以及 `tests/profile.rs::cli_lbr_profile_rejects_missing`。

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

dcc 当前仅定义配置 + 数据结构 + 简化读取器。
不在 libc malloc 上插桩 (那需要弱符号覆盖, 当前没有 runtime 库)。

### 5.2 简化格式

```
# header
alloc <frames...> = <count> <bytes>
lifetime <alloc_frames...> -> <dealloc_frames...> = <bytes>
```

### 5.3 `is_cold` 启发

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

### 6.3 测试

`topdown_percent_sum_to_one`，以及
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

### 7.5 测试

单元：`src/profile/offload.rs` 6 个（json_roundtrip /
binary_roundtrip / bad_magic / dedup / arch_to_triple_mapping /
validate_reports_bad_value）。集成：`tests/profile.rs` 11 个 offload
用例（CLI 全形式解析与非法值拒绝、`.dcc.offload` 节内容与位置断言、
E0310 / W0311-13 位置、HIP arch 映射），fixtures 为
`tests/fixtures/c/offload_{omp,cuda}.c`。

---

## 8. 集成测试

文件：`tests/profile.rs`，共 11 个测试：

| 测试 | 覆盖 |
|------|------|
| `cli_profile_generate_is_accepted` | `-fprofile-generate=dir` + 编译出 `.o` |
| `cli_profile_use_missing_path_rejects` | `-fprofile-use` 无 path 报错 |
| `cli_emit_asm_still_works_with_profile` | `-fprofile-arcs` + `-fsyntax-only` 共存 |
| `cli_test_coverage_accepted` | `-ftest-coverage` 接受 |
| `cli_record_profile_rejects_missing_path` | `-frecord-profile` 无 path 报错 |
| `cli_fopenmp_accepted` | `-fopenmp` 接受 |
| `cli_fopenmp_targets_accepted` | `-fopenmp-targets=nvptx64-nvidia-cuda` 接受 |
| `cli_fcuda_and_fhip_accepted` | `-fcuda -fhip` 接受 |
| `cli_fmemprof_accepted` | `-fmemory-profile` 接受 |
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

共 10 个，覆盖：profraw roundtrip / weights aggregate / gcov notes
magic / gcov data roundtrip / autofdo parse + bucketize / lbr brstack
parse / memprof parse + is_cold / topdown sum / offload json roundtrip。

---

## 9. 已知限制与后续工作

* **真实插桩路径未接 codegen**：`pgo::instrument_program` 改写了 AST，
  但 driver / codegen 阶段尚未在每个函数 prologue emit `inc <counter>`
  或调用 `__dcc_pgo_count_fn`。当前 `-fprofile-generate` 编译能过，
  但运行期不会自动落 `.profraw`，需用户在入口手动调用。
* **AutoFDO / LBR / MemProf 权重不下发到 codegen**：数据结构 + parser
  就绪，但 hot/cold 重排 / inliner hint / `__attribute__((malloc))`
  属性注入等下游通路还在 backlog。
* **Offloading 设备后端未实装**：只接受 `-fcuda` / `-fhip` /
  `-fopenmp-targets=` 选项并存元数据，device codegen 路径不存在。
* **AArch64 / RISC-V profile 后端**：当前 PGO / GCOV 计数器仅在
  x86_64 上验证 layout，其它 arch 待补。
* **.dcc.offload 二进制格式**：当前是 JSON，binary 序列化待定。

后续 milestone (M21+ 候选)：
* 把 `instrument_program` 接到 driver 的 `post_parse` 钩子，真正落计数
* AutoFDO 权重 → 函数布局 / inliner 提示
* MemProf `is_cold` → `__attribute__((malloc))` 注入
* Offloading 至少完成 OpenMP `target` 的 device dispatch stub
* AArch64 / RISC-V profile runtime helper

---

## 10. 相关文档

* `docs/roadmap.md` — 里程碑 M0–M19
* `docs/architecture.md` — 整体架构
* `docs/performance.md` — 性能模型 + 加速来源
* `docs/missing_features.md` — 缺失特性与已知限制
* `docs/support_features.md` — 已支持特性清单
* `src/profile/mod.rs` — `ProfileConfig` 定义
