# 后端拆分与多架构一致性改造方案

> 版本：**v1.2**（v1 方向定稿；v1.1 源码复核；v1.2 防腐 + 语义单源试点，不改阶段 0–3 施工顺序）  
> 状态：方案稿 — **尚未开始改代码**。  
> 摸底范围：`src/codegen/*`、`src/target.rs`、`src/cli.rs`、`docs/architecture.md`（2026-09）。  
> 目标：拆分 `x86_64.rs`、统一多架构后端契约，使能力一致、可度量、可卸载；并建立**不回潮**机制与**语义收敛**入口。  
> 关联：`docs/architecture.md`、`docs/diagnostics.md`、`docs/missing_features.md`、  
> `docs/cpp_test_naming_unification_plan.md`、`docs/next_steps.md`（内核 P0 优先级）。  
> 方案路径：**`docs/backend_refactor_plan.md`**。  
> **能力边界（v1.2 写死）**：本方案主战场是 `codegen::emit` / 对象生成；**不承诺** link、LTO 优化质量、sanitizer 运行时、C++ native 端到端与 x86 完全等价——Those 另立清单或矩阵子表。

---

## 0. 现状诊断

### 0.1 规模严重失衡

| 文件 | 约行数 | 体积 | 备注 |
|------|--------|------|------|
| `codegen/x86_64.rs` | **39,562** | **~1.9 MB** | ~612 个 `fn`，几乎全在一个 `impl Codegen` |
| `codegen/aarch64.rs` | 6,598 | ~287 KB | ~280 fn |
| `codegen/riscv64.rs` | 6,256 | ~266 KB | ~310 fn |
| `codegen/arm.rs` | 2,569 | ~106 KB | **内部**可 `build_elf32_*`（打包点不统一） |
| `codegen/mips.rs` | 2,205 | ~91 KB | |
| `codegen/mips64.rs` | 86 | ~3 KB | `Mips64Codegen { inner: MipsCodegen }` 薄封装 |
| `codegen/mod.rs` | 2,104 | ~82 KB | asm 校验 + `emit()` 约 7 臂（含 RV32 stub） |
| `codegen/mini_asm.rs` | 1,344 | ~51 KB | 非 x86 共用 mini 汇编器 |
| `target.rs` | 1,067 | — | 数据驱动目标描述（方向正确） |

**不存在** `src/codegen/riscv32.rs`；`src/riscv32.rs` / `src/thumb.rs` 在 `src/` 根，不在 `codegen/` 编排内。  
x86 单文件 ≈ 其余主要后端之和的 2 倍以上。函数名前缀粗分：`emit_*` ~218、`gen_*` ~75、asm 相关 ~30+。

关键单函数（拆分时必须再切）：

| 符号 | 约起止行 | 约行数 |
|------|----------|--------|
| `gen_expr` | 9883–14343 | **~4.4k** |
| `gen_stmt` | 9006 起 | ~800 |
| `emit_sysv_call` | 8249 起 | 数百 |
| `compile` / `compile_exec` / `compile_rel` | 3503 / 3646 / 4535 | 帧与整体编排 |

### 0.2 架构名言 vs 实际

`src/target.rs` 注释：

> 后端是"数据驱动"——不同架构共享一套 IR，只换指令编码表。  
> 不为每种架构写一个 `codegen.rs`；而是 codegen 跑在 target 描述之上。

**现实**：

1. **没有 `Backend` trait** — `codegen::emit()` 按 `Arch` 分支；类型不统一：`x86_64::Codegen` / `aarch64::Aarch64Codegen` / `mips::MipsCodegen`…
2. **能力深度不一致** — 关键词粗测**不可直接当功能矩阵**（arm/mips 某项 0 可能来自命名/共享路径），须用行为探针（见阶段 0）。
3. **`has_backend()` 全为 `true`，且与 emit 语义冲突（v1.1 硬伤）**：

   | Arch | `has_backend()` | `emit()` 实际 |
   |------|-----------------|---------------|
   | RiscV32 | **true**（cli 注释仍写 Phase 7） | **无条件 `Err(E0399)`「尚未实现」** |
   | 其余 6 | true | 进入对应分支 |

   语义从「有完整后端」退化成「能进 match」，RV32 更是「能进分支然后必失败」。README / `architecture.md` / `cli.rs` 文档字符串仍写「完整」。

4. **后处理结构相似但细节分叉**（不是均质「7 段复制粘贴」）：
   - ELF rel/exec、部分 LTO section：结构相近  
   - DWARF：各 arch 自建 `func_pcs` / `line_entries` / `ArchKind` / CIE 参数（riscv 存在 `pc+64` 一类 per-arch 假设）  
   - `build_asm_output` 签名不统一（x86 偏 `&Program`/source，aarch64 等偏 `&Module`）  
   - **打包点不统一**：arm 在 `compile` 内 `build_elf32_*`；x86 在 `emit()` 外 `build_elf_*`  
   - COFF：仅 x86 + Windows  
   - `offload` / `line_map` / `function_sections` 等仅部分分支用满  

5. **诊断失败条件不一致（parity 真 bug）**：
   - x86_64：仅 `Severity::Error` 时失败  
   - aarch64 / riscv64 等：`!diagnostics.is_empty()` 即 `Err`  
   → 同一批 warning：x86 可过，交叉可能整单失败。

6. **测试不对称**：x86 深测（sanitizer/asm/内核构造）；交叉多为 `*_hello_cross_compile` + 少量 HFA/atomic；缺「同一 IR 特性 × 全 arch」同源表。  
   现有 cross 相关集成用例约 35 量级（名称含 cross），仍偏 smoke。

7. **文档绑行号**：`architecture.md`、`array_index_addressing_refactor_plan.md`（`#L…`）、README 表等，拆分后集体失效。

8. **`parallel_codegen` 实际仅 x86**（`emit` 里也只在 x86 臂赋值给 `cg.parallel_codegen`）。

### 0.3 根因（一句话）

**IR 共享，「IR → 机器码」契约不共享**：私有 `compile()` + 私有字段 + 私有/半私有后处理 → (a) x86 膨胀，(b) 新特性默认只改 x86，(c)「平台支持」无法同一标准度量。

---

## 1. 目标与非目标

### 1.1 目标

1. **拆开 `x86_64.rs`**：无「无正当理由的」单文件 > 5k 行（`gen_expr` 类需内部再拆）。  
2. **统一后端契约**：同一套 API；能力显式；缺则显式拒绝。  
3. **功能一致性可度量**：行为探针矩阵 + FeatureSet + 同源测试。  
4. **可卸载平台**：注册表一处关，同步 `has_backend` / CI / 文档，无死臂。

### 1.2 非目标（一期不做）

- 不重写指令选择；不引入 LLVM/MLIR。  
- 不一次抽空跨后端语义到完美 machine IR（二期 RFC）。  
- 不强行补齐 arm/mips 全部高级特性（先标记再补或硬报错）。  
- 不改 C++ frontend / linker 主逻辑。  
- **一期不上 7 个 `backend-*` cargo feature**（见 §4.5；避免 CI 组合爆炸）。

### 1.3 成功标准（验收）

| 标准 | 度量 |
|------|------|
| 文件规模 | `x86_64/` 下常规模块 < 5k 行；超限须在方案附录列例外并说明 |
| 契约 | `trait Backend`；Tier-1 arch impl；包装期行为与现网一致 |
| 能力可见 | `docs/backend_feature_matrix.md` = 行为探针结果 = `FeatureSet` |
| 同源测试 | `tests/backend_parity.rs` 按 L0/L1/L2 分层，见 §4.3 |
| 诊断一致 | 全 arch「何为编译失败」有单一策略 + 对照测试 |
| RV32 | `has_backend=false` 或真正实现；**禁止** true+E0399 长期并存 |
| 卸载 | registry 摘除 → `has_backend=false` → `emit` 臂删除 → CI/文档同步 |

---

## 2. 目标架构

```text
                    ┌─────────────────────┐
                    │  driver / codegen   │
                    │  emit(module,target)│
                    └──────────┬──────────┘
                               │  registry::backend_for(arch)
                               ▼
              ┌────────────────────────────────┐
              │  trait Backend                 │
              │  + features() -> FeatureSet    │
              │  + compile(CompileInput)       │
              │  + render_asm / into/package   │
              └────────────┬───────────────────┘
         ┌─────────┬───────┼────────┬─────────┐
         ▼         ▼       ▼        ▼         ▼
      X86_64    AArch64  RiscV64  Arm      Mips
      (mod/*)   (mod/*)  (+32?)   (mod)   (+Mips64 wrap)
         │         │       │        │         │
         └─────────┴───────┴────────┴─────────┘
                    shared: ir / target / elf / mini_asm
                    二期可选: machine::{Frame,Calling,Reloc}
```

### 2.1 分层

| 层 | 内容 | 共享程度 |
|----|------|----------|
| L0 IR/语义 | `ast` / `ir` / `ssa` / `opt` | 与 arch 无关 |
| L1 目标描述 | `Target` | 数据 |
| L2 后端契约 | `Backend` + `Feature` + `CompileInput/Output` | **受支持 arch 必须满足** |
| L3 各 arch lowering | 寄存器、ABI、指令发射 | 每 arch（允许 Mips64 组合复用） |
| L4 打包 | ELF/COFF、reloc、section、debug 胶水钩子 | **逐步从 emit 上收** |
| L5 测试 | 同源用例 + 分层期望 | 矩阵驱动 |

---

## 3. Part A — 拆分 `x86_64.rs`

### 3.1 原则

1. **先切模块，再抽公共**：一 PR 只「移动 + 可见性」，不改语义。  
2. 按 **职责簇** 切，不按行号硬切。  
3. 可见性：`pub(crate)` / `pub(super)`；`struct Codegen` 字段留 `mod.rs`，多文件 `impl Codegen`。  
4. 对外 `pub use` 保兼容；`emit` 签名变更与拆分 PR **分离**。  
5. **禁止**在拆分 PR 修功能；行为 diff = 失败。  
6. 单测：原 `mod tests` 若过少（x86 内 `#[test]` 稀疏），「tests 随文件走」需配套 **从集成层回补** 策略，见 §3.7。

### 3.2 目标目录（完整版）

```text
src/codegen/x86_64/
  mod.rs           # Codegen 结构体、new/compile 总入口
  emit/
    mod.rs
    frame.rs
    call.rs
    stmt.rs
    expr/
      mod.rs       # gen_expr 分发
      lvalue.rs
      binop.rs
      call.rs
      cast.rs
      …            # gen_expr ~4.4k 行必须内拆，禁止整块塞进单文件
    addr.rs
    load_store.rs
    int128.rs
    vector.rs
    float_x87.rs
    init.rs
    builtin.rs
    sanitizer.rs
    tls.rs
    instrument.rs
  asm/
    reg.rs / constraint.rs / plan.rs / template.rs
    label.rs / directive.rs / emit_asm.rs
  abi/
    sysv.rs / win64.rs
  elf_side/
    reloc_collect.rs / parallel.rs
  tests/
```

### 3.3 一期最小拆分（更粗的 PR 刀）

| 模块 | 内容 | 约行量 | 备注 |
|------|------|--------|------|
| `mod.rs` | 结构体 + compile 编排 | 3–4k | |
| `emit/expr`（目录） | gen_expr **及子文件** | 单文件仍须 <5k | 见 expr/ |
| `emit/stmt_init.rs` | gen_stmt / init | 3–5k | |
| `emit_mem.rs` | load/store/addr/global | 5–6k | |
| `emit_special.rs` | int128/vector/x87/sanitizer/tls | 5–6k | |
| `emit_call_frame.rs` | 调用/帧/返回 | 4–5k | |
| `asm.rs` 或 `asm/` | inline + x86 mini-asm | 6–8k | **建议第一刀** |
| `abi.rs` | SysV/Win/va | 2–3k | |
| `parallel.rs` | 并行 emit | 1–2k | |
| `tests.rs` | 单测 | 1–2k | |

### 3.4 落地顺序（纯移动）

```text
PR-A1  建 x86_64/ 目录，整文件进子模块壳（行为零变）
PR-A2  抽出 asm/*          ← 可与阶段 0 并行，不依赖 emit 统一
PR-A3  抽出 abi + call/frame
PR-A4  抽出 mem/addr
PR-A5  抽出 special（int128/vector/x87/sanitizer/tls）
PR-A6  抽出 parallel + elf 收集
PR-A7  expr 内部再拆（lvalue/binop/cast/…）直至单文件达标
PR-A8  测试分模块；文档行号 → 符号路径（清单见 §3.7）
```

**与阶段 1–2 的依赖**：

| 工作 | 依赖 |
|------|------|
| 阶段 0 矩阵/基线/RV32 修正 | 无 |
| PR-A2 asm 拆出 | 与阶段 0 **可并行** |
| 阶段 1 emit 后处理 | 串行；动 `emit` 签名的 PR 不与 A 系列大拆同一周混并 |
| 阶段 2 trait | 依赖阶段 1（至少诊断策略 + 输出类型方向） |
| PR-A3+ | 可与阶段 1 并行，但 **freeze 窗口** 内只合纯移动 |

每步门禁：

```bash
cargo build --tests
cargo test --test integration
# 固定 fixture / bootstrap 字节级对比（见阶段 0 基线）
bash scripts/bootstrap.sh   # 环境允许时
```

### 3.5 已有共享、应加固

| 模块 | 动作 |
|------|------|
| `asm_constraints.rs` | 保持共享；x86 特有进 `x86_64/asm/` |
| `mini_asm.rs` | 非 x86 共用 |
| `elf.rs` / `coff.rs` | 打包层；统一调用点见 Part B |
| `disasm.rs` | 测试/诊断，不进 lowering |
| `Mips64` 组合模式 | **写入契约实现指引**：薄 arch 用 inner 复用，不抄 2k 行 |

### 3.6 可见性与死代码

- 避免无节制 `pub`；`#[allow(dead_code)]` 清理放 A 系列最后。  
- 拆分后跑一次 `cargo build --tests` 零告警基线，防止 allow 滥用。

### 3.7 文档行号清单（拆分 PR-A8 必须改完）

| 文件 | 内容 |
|------|------|
| `docs/architecture.md` | `gen_stmt` / `gen_expr` 等行号 → `x86_64::emit::…` 符号 |
| `docs/array_index_addressing_refactor_plan.md` | 多处 `#L…` |
| `README.md` | 后端表与 `has_backend` 叙述（与阶段 0 重叠） |
| `docs/support_features.md` | 若含 `x86_64.rs#` 引用 |
| `docs/diagnostics.md` | 代码位置列表 |
| 其它 `rg 'x86_64\.rs#' docs` 命中 | 一并处理 |

**约定**：此后文档只允许 **文件相对路径 + 符号名**，禁止再写绝对行号。

---

## 4. Part B — 多架构功能一致

### 4.1 统一 `Backend` 契约（两阶段，防类型墙）

**一期（适配）**：

```rust
// src/codegen/backend.rs
pub struct EmitOptions {
    // 必须与当前 emit() 参数集对齐（勿裁剪）：
    pub relocatable: bool,
    pub emit_asm_text: bool,
    pub source_name: String,
    pub debug_level: u8,
    pub dwarf_version: u8,
    pub sanitizers: SanitizerFlags,
    pub lto_enabled: bool,
    pub function_sections: bool,
    pub parallel_codegen: bool,
    pub bounds_check: bool,
    pub trapv: bool,
    pub offload: Option</* 需拥有或 &'a */>,
    pub line_map: &'a [LineSpan],   // 或内部复制
    pub verbose: bool,
    // … 与现签名逐项对照后冻结
}

pub struct CompileInput<'a> {
    pub module: &'a ir::Module,
    pub target: &'a Target,
    pub opts: EmitOptions<'a>,
}

/// 各 arch 先提供显式映射，避免 Box<dyn Any>
pub struct CommonImage {
    pub code: Vec<u8>,
    pub rodata: Vec<u8>,
    pub data: Vec<u8>,
    pub bss: u64,
    pub tls: TlsImage,              // 统一字段，或 enum 保真
    pub symbols: ObjectSymbols,     // 适配期可 enum { Elf(..), ... }
    pub relocs: ObjectRelocs,
    pub extra_sections: Vec<ExtraSection>,
    pub diagnostics: Vec<Diagnostic>,
    pub func_sizes: BTreeMap<String, u64>,
    // debug / comdat / parallel 结果按需扩展
}

pub trait Backend {
    fn arch(&self) -> Arch;
    fn features(&self) -> FeatureSet;
    fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>>;
    fn render_asm(&mut self, input: &CompileInput<'_>) -> Result<String, Vec<Diagnostic>>;
}
```

**适配期关键**：

1. 各 arch `into_common_image()` **显式字段映射**，禁止笼统 `…`。  
2. **打包点统一**：凡在 `compile` 内 `build_elf*` 的（arm 等）先改为产出 image，**ELF/COFF 一律在共享 `package()`**（或 `Backend::package` 默认实现）。  
3. **诊断策略先统一**（见 4.1.1），再进 trait。

#### 4.1.1 诊断失败策略（阶段 1 必做）

- 单一函数：`fn has_fatal(diags) -> bool`。  
- **建议**：仅 `Severity::Error` 失败（对齐现 x86；与 driver 可组装规则一致）。  
- 对照测试：同一批 warning 程序在 x86 与 aarch64 上 **同过/同挂**。  
- 禁止继续「空 diagnostics 才算成功」。

#### 4.1.2 `emit()` 目标形态

1. `registry` / `has_backend` 校验（缺 → **`Err`，禁止 `panic!`**）  
2. `backend.compile`  
3. 共享：`has_fatal`、LTO section 注入、debug 钩子、`package_elf`/`coff`  
4. 返回字节  

**体量预期（v1.1 修正）**：`mod.rs` ~2100 → **~900–1200**（非 600–800）；  
Phase 1 **拆 2–3 个 PR**，勿一口吃成。

### 4.2 `Feature` / `FeatureSet`

```rust
// src/codegen/features.rs
#[non_exhaustive]
pub enum Feature {
    CoreArith, CoreCall, Float, LongDouble, Int128, Bitfield,
    VaList, Tls, InlineAsm(AsmLevel), Vector, Atomics,
    Sanitizer(SanKind), CfiUnwind, StackProtect,
    ParallelCodegen,   // 实测：仅 x86
    Lto, BoundsCheck, Trapv,
    ObjectFormat(ObjectFmt), // Elf / Coff；Coff 仅 x86+Win
    Dwarf,                   // 调试信息深度
    // …
}
```

| 级别 | 含义 | 用户可见 |
|------|------|----------|
| **Tier-1** | 内核 / Lua / libiberty 深度，CI 全开 | 默认选项可用 |
| **Tier-2** | hello / cross smoke | 高级特性 → **硬错误 E0xxx** |
| **Disabled** | registry 摘除 | `has_backend=false` + 明确文案 |

规则：**宁可 E0xxx，不可静默 no-op**（与 `missing_features.md` 中「有意 no-op 的旗子」分开列表）。

`has_backend()`：改为 **registry / 静态表同源**，删除手写全 true；修正 `cli.rs` 注释中「完整 codegen」表述。

### 4.3 同源测试 — L0/L1/L2（v1.1）

交叉 `run rc=0` 在 Windows 本地不可用，**禁止**全部压成一种期望：

| 层 | 断言 | 环境 |
|----|------|------|
| **L0** | `--target=<arch> -c` 成功；ELF class/machine 正确；或期望 `Err(E0xxx)` | 本地 Windows + Linux |
| **L1** | qemu-user / 对应 runner 上 `rc=0` | CI Linux |
| **L2** | 反汇编/golden 字节（默认 **仅 x86 + 少数样本**） | CI |

```text
tests/backend_parity.rs
  table: Feature × arch × fixture × {L0,L1?,expect}
  arch 能否跑 L1 来自 FeatureSet/Tier，测试体不散落 if-arch
  L1 不可用时：skip 并记 metric，而不是 ignore 整表
```

**黄金字节**：阶段 0 对 10–20 个小 TU 记录 x86 `.o` sha256（含 `.text` 哈希），供 A 系列回归。

### 4.4 共享边界

**优先共享**：`Target`、DWARF abs reloc（已有 `debug_abs_reloc_type`）、ELF 打包与 LTO 注入、Feature/诊断模板、parity harness、**统一失败策略**。

**留在 arch**：指令编码、寄存器分配、ABI 分类、TLS/asm 方言、CIE 细节（经 hook 参数化）。

**二期可选**：语义 lowering → machine IR；**不与拆 x86 同阶段**。

### 4.5 注册表与卸载 — **一期不用 cargo backend-\* features**

```rust
// src/codegen/registry.rs
pub fn backend_for(arch: Arch) -> Option<Box<dyn Backend>> {
    match arch {
        Arch::X86_64 => Some(/* … */),
        Arch::AArch64 => Some(/* … */),
        Arch::RiscV64 => Some(/* … */),
        Arch::Mips | Arch::Mips64 => Some(/* … */),
        Arch::Arm => Some(/* … */),
        Arch::RiscV32 => None,  // 与 has_backend 同源
        // 卸载：整臂删除并同步 has_backend()
    }
}
```

| 一期 | 二期可选 |
|------|----------|
| 静态 match + `const`/`has_backend` 同一源 | `cfg(feature = "backend-arm")` 等瘦身发行 |
| 卸载 = 删臂 + 改 has_backend + CI + 文档 | 多 feature 组合矩阵、文档生成 |

理由：当前仅 `gcc_src` 一个 feature；7 个 backend feature 的 CI/文档成本高于一期收益。

卸载清单：registry → `has_backend` → **删除 `emit` 臂**（防 `a => panic!`）→ CI matrix → 文档 Disabled → parity 表自动收缩 → 清残留。

### 4.6 阶段 1 子 PR 拆分（v1.1）

| 子 PR | 内容 | 预期 |
|-------|------|------|
| 1a | 诊断失败策略统一 + 对照测试 | 行为修正（有意变更需 changelog） |
| 1b | `EmitOptions` 参数打包（签名迁 struct，逻辑不动） | 减参，行为不变 |
| 1c | LTO / section 注入上收 | 去重 |
| 1d | debug 胶水 `build_debug_hook(arch, …)` 抽出 | 最难，允许 arch 保留钩子 |
| 1e | elf/coff `package()` 上收（含 arm 去内嵌 build_elf） | 打包点唯一 |

**禁止** 1a–1e 塞进同一个 PR。

---

## 5. 分阶段路线图

| 阶段 | 内容 | 产出 | 风险 | 粗估 |
|------|------|------|------|------|
| **0 基线** | RV32 `has_backend` 修正；**行为探针**矩阵；失败语义结论；`.o`/`.text` 快照；P0 用例选型；README/cli/architecture「完整」表述对齐 | `backend_feature_matrix.md` 初稿 | 低 | 0.5–1 人日 |
| **1 emit 统一** | §4.6 子 PR：诊断→Options→LTO→debug→package | `mod.rs` ~900–1200 | 中 | 2–4 人日 |
| **2 契约** | `into_common_image` 适配 + `trait Backend` + FeatureSet + 缺特性 E0xxx | `backend_parity` L0 绿 | 中 | 2–3 人日 |
| **3 拆 x86** | §3.4；A2 可提前并行 | `x86_64/` 模块树 | 中 | 3–6 人日 |
| **4 补洞** | Tier-2→1；arm/mips 补或硬报错 | 矩阵与 README 一致 | 持续 | — |
| **5 可选** | machine IR；**可选** cargo backend features；**语义单源扩大**（§12.3） | — | 高 | 独立 RFC |

### 5.0 阶段与 v1.2 的关系（不打乱 0–3）

- 阶段 **0–3 顺序与子 PR（1a–1e、A1–A8）不变**。  
- **§12.1 防腐**：与阶段 0 **同一批**落盘（探针脚本 + CI job + PR checklist），否则矩阵当日即腐。  
- **§12.2 语义单源试点**：排在 **阶段 1 完成之后、阶段 2 期间或之后**（建议 Phase 2 收尾并行开一条小线），**禁止**与 A 系列纯移动、1a 行为变更抢同一文件周。  
- **§12.4 Tier-1 产品清单**：阶段 0 输出初版，阶段 4 才有「补到哪」的 DoD。

### 5.1 与项目 P0 的关系（必须遵守）

- `docs/next_steps.md` **P0 = 内核全树**（`make CC=dcc` → vmlinux/bzImage）。  
- 本方案定级 **P1/P2 维护性投资**。  
- **内核红灯排障期不开阶段 3 大拆**；阶段 1 只在冲刺间隙合。  
- 与 C++ native 大改 `x86` 路径 **错峰**（阶段 3）。  
- **freeze 窗口**：任一「纯移动」PR 合并前，不开其它同文件功能 PR。

---

## 6. 风险与缓解

| 风险 | 缓解 |
|------|------|
| 拆文件丢字段/测试 | 纯移动 + 字节/集成门禁；禁止顺手改逻辑 |
| trait 过早抽象 | 先 `into_common_image` 适配，不强迫改内部 lowering |
| `CompileOutput` 类型墙 | enum/CommonImage 显式映射，禁止 `…` 交付 |
| emit 拆分低估 debug 胶水 | 1d 独立 PR + arch 钩子保留 |
| 诊断统一改行为 | 1a 单独 PR、写明 breaking、对照测试 |
| 字节级回归 | bootstrap + 阶段 0 golden `.text` hash |
| Windows 跑不了交叉执行 | parity 分 L0/L1/L2；L1 仅 CI |
| 关键词矩阵误导 | **行为探针**为准，关键词仅辅助 |
| cargo feature 爆炸 | 一期不用 backend-* features |
| 与内核 P0 / C++ 冲突 | §5.1 错峰与 freeze |
| `gen_expr` 仍巨物 | A7 强制 expr/ 子拆；验收盯单文件 |
| 过度设计 | 静态 registry + 数据矩阵；无动态插件 |
| **矩阵与 has_backend 同样腐烂** | **§12.1** CI diff + PR checklist，文档禁止手改唯一真相 |
| **只统一门面、语义仍五套** | **§12.2** 最小语义单源试点；阶段 5 才扩面 |
| **Tier-2 标完即算「完成」** | **§12.4** 产品化 Tier-1 清单 + 阶段 4 DoD |
| **parity 抽样当全等** | **§12.5** 验收分「度量一致 / 语义抽查 / 能力拉平」三级，不混谈 |
| 被读成全工具链一致 | 文首能力边界 + §12.6 明确 out of scope |

---

## 7. 文档与 PR 切片

### 7.1 文档落点

| 文件 | 内容 |
|------|------|
| **`docs/backend_refactor_plan.md`** | 本文（**v1.2**） |
| `docs/backend_feature_matrix.md` | 阶段 0：探针结果 + Tier |
| `docs/architecture.md` | Backend 契约；**废除行号** |
| `docs/diagnostics.md` | arch 不支持特性 E0xxx；统一失败语义说明 |
| `docs/missing_features.md` | 分「有意 no-op」vs「后端未实现 → 硬错误」 |
| `README.md` | 「全部完整」→ 矩阵链接；RV32 如实描述 |

### 7.2 建议提交信息

```text
fix(cli): RiscV32 has_backend=false to match emit E0399
refactor(codegen): unify fatal-diagnostic policy across arches
refactor(codegen): EmitOptions bundle + LTO package extract
refactor(codegen): extract package_elf from per-arch emit arms
refactor(codegen): split x86_64 into modules (asm|frame|expr/…)
test(backend): behavior matrix + parity L0 table
test(ci): check_feature_matrix probe vs docs diff
docs: backend feature matrix; replace x86 line anchors with symbols
docs: Tier-1 arch×feature product checklist
refactor(semantic): shared lowering helpers pilot (short-circuit|compound-assign)
```

---

## 8. 最小可执行清单（开工顺序）

1. **阶段 0**：RV32 `has_backend`；行为探针矩阵；**防腐 job + PR checklist（§12.1）**；诊断策略调研；golden hash；**Tier-1 初版（§12.4）**；改 README/cli/architecture 表述。  
2. **1a**：统一 `has_fatal` + 对照测试。  
3. **1b–1e**：Options / LTO / debug 钩子 / package（分 PR）。  
4. **FeatureSet + registry + parity L0（含显式 Err 断言）**；确认 `check_feature_matrix` 在 PR 上必过。  
5. **PR-A2 asm 拆出**（可与 0–1 并行，freeze 约束下）。  
6. 其后 A3–A8；expr 内拆直到单文件达标。  
7. **语义单源试点（§12.2）**：阶段 2 收尾并行一条小线；通过后才谈扩大。  
8. 合并策略：只合 **纯移动** 或 **纯测试/文档** 或 **单主题行为 PR**，不混。

---

## 9. 与平台退役的协同

| 场景 | 操作 |
|------|------|
| 支持但特性残缺 | FeatureSet 缺位 → 使用点 **E0xxx**；矩阵 partial |
| 整平台退役 | 删 registry 臂 + `has_backend=false` + 删 `emit` 臂 + CI/文档/测试同步 |
| 只维护 `dcc -c` | Tier 降级并写清；禁用 LTO/sanitizer 等 |
| RV32（现状） | **阶段 0 立即对齐** false，或真正实现后改 true |

---

## 10. v1 → v1.1 修订记录

| # | 问题 | 修订 |
|---|------|------|
| 1 | 未写 RiscV32 `has_backend=true` + `emit` 必 E0399 | §0.2、§9、阶段 0 必做 |
| 2 | 未写诊断 Error vs any 不一致 | §0.2、§4.1.1、1a |
| 3 | 「七段复制粘贴」/「mod→600–800」过乐观 | 改为结构相似+debug 分叉；900–1200；1a–1e |
| 4 | arm 等 compile 内嵌 build_elf 未提 | 打包点统一 1e |
| 5 | EmitOptions/CompileOutput 草图不完整 | 全参数对齐 + `into_common_image` |
| 6 | 矩阵靠关键词 | 阶段 0 **行为探针** |
| 7 | 交叉 run rc=0 不现实 | parity **L0/L1/L2** |
| 8 | cargo backend-* 一期取消 | §4.5 |
| 9 | 阶段强制串行 | A2 可并行；依赖表 |
| 10 | gen_expr 4.4k 未内拆 | expr/ 目录 + A7 |
| 11 | 文档行号清单不全 | §3.7 |
| 12 | 未对齐内核 P0 | §5.1 |
| 13 | mips64 薄封装未写成模式 | §3.5 |
| 14 | 验收「均 impl」空洞 | Tier + 诊断 + RV32 |
| 15 | 表内 riscv32.rs 行数错误 | 删除；说明无此文件 |
| 16 | panic 兜底 | 改为 Err |
| 17 | ObjectFormat/Parallel 仅 x86 | Feature 枚举注明 |
| 18 | 自指「本文档路径」混乱 | 固定 `docs/backend_refactor_plan.md` |
| 19 | 粗估工时缺失 | §5 表 |
| 20 | x86 单测稀疏 | §3.1 tests 回补策略 |

### 10.1 v1.1 → v1.2 修订记录

| # | 问题（审查结论） | 修订 |
|---|------------------|------|
| 21 | 方案不能单独保证「做完=语义/能力一致」 | 文首边界；§12 分级验收；结论改为「度量+收敛+流程」 |
| 22 | 矩阵会像 `has_backend` 一样腐烂 | **§12.1** 探针 CI、文档单一真相、PR checklist |
| 23 | 缺语义单源，只统一门面 | **§12.2** 最小试点（2–3 语义、2–3 arch）；阶段 5 扩大 |
| 24 | Tier-2 硬报错易被当成完成 | **§12.4** 产品化 Tier-1 清单与阶段 4 DoD |
| 25 | 边界不清（link/debug/C++） | 文首 + **§12.6** out of scope |
| 26 | 无 owner / 完成定义 | §12.4、§12.7 角色 |
| 27 | 拆分后无性能/体积回归 | §12.5 L2 扩展：`.text` 体积与 `-O2` 编译耗时基线 |
| 28 | 阶段 0 未含防腐 | **§5.0** 防腐与阶段 0 同批 |

---

## 11. 一句话结论

**拆 `x86_64.rs` 解决「太胖」；统一 `Backend` + `Feature` + 分层 parity 解决「各平台各做各的、还假装全支持」；§12 解决「做完会不会回潮、以及能不能碰语义一致」。**  
v1.2 起：阶段 0–3 施工顺序不变，但 **矩阵必须 CI 化**、**语义单源先做 1–2 个试点**、**Tier-1 按产品清单验收**——否则最多得到「看得见的不一致」，得不到「收敛中的一致」。

---

## 12. v1.2：防腐机制与语义单源最小试点

> 本节回答：「按前文做完，多架构一致性是否站得住？」  
> 答案分三级：**度量一致（必须）→ 语义收敛（试点起步）→ 能力拉平（按 Tier-1 清单，非自动）**。  
> **不改动** §5 阶段 0–3 的 PR 切片与门禁定义，只增加并行约束与验收附件。

### 12.1 防腐：矩阵不许再变成手写谎言

`has_backend` 全 true 的根因是 **文档/枚举与 emit 行为无机械关联**。`FeatureSet` 若重复此模式，v1.1 全部施工会在半年内失效。

| 机制 | 做法 | 阶段 |
|------|------|------|
| **行为探针为真相源** | `scripts/probe_backend_features.sh`（或 `cargo test --test backend_matrix_probe`）：对每 arch × fixture 列表跑 `--target -c`，输出 JSON/TSV：`arch,feature,status=ok\|err_code\|missing_fixture` | 阶段 0 |
| **CI 强制 diff** | job **`check_feature_matrix`**：探针输出 vs `docs/backend_feature_matrix.md` 表格（或由探针生成的 `matrix.gen.md` 段落）不一致 → **失败**。禁止只手改 md 不改代码 | 阶段 0 与探针同批 |
| **文档单一真相** | README / `architecture.md` 表格 **只许链接矩阵文件**，禁止再维护第二份 arch×能力表；`cli.rs` 注释与矩阵同一次 PR 改 | 阶段 0 |
| **代码侧同源** | `FeatureSet` 静态表在 `features.rs`；**单测** `assert!(features_equals_probe_fixture_list())` 或 build-script 生成 `features_generated.rs`（二期）；一期最低要求：parity 表的 arch 过滤读同一 `FeatureSet` | 阶段 2 |
| **PR checklist** | 模板勾选：① 是否新增/修改 **语义或 codegen 路径**？② 是否更新 **探针 fixture + 矩阵**？③ 是否给 **支持 arch 列表** 中每个 arch 加了 L0（或显式 `Err`）期望？④ 是否动 `has_backend`/registry？ | 阶段 0 进 `.github/PULL_REQUEST_TEMPLATE.md` |
| **CODEOWNERS（可选）** | `src/codegen/**` 与 `backend_feature_matrix.md` 同 reviewer 组，防 docs-only 绿、代码红 | 阶段 0–1 |
| **禁令** | ① 禁止 CI 外「更新矩阵」；② 禁止 `Feature::X` 出现却无探针行；③ 禁止 `has_backend` 手写列表（改 registry 同源）；④ 禁止长期 `#[ignore]` 的 parity 主表行（`skip` 须带 issue 号） | 全程 |

**阶段 0 交付物增加**（原矩阵初稿旁）：

1. 探针脚本/测试入口  
2. `check_feature_matrix` CI 配置（e2e 或独立 workflow）  
3. PR 模板段落  
4. 矩阵文件头注明：`generated-or-verified-by: probe @ <commit>`  

### 12.2 语义单源：最小试点（不 = 完整 machine IR）

**问题**：`trait Backend` 统一进出口，**不**统一 `gen_expr`/`gen_stmt` 里的 C 语义；短路、复合赋值求值次数、cast 次序等可在多 arch 静默分叉，parity 抽样测不到。

**原则**：

1. **试点体量极小**：2–3 条语义 × 2–3 个 arch（x86_64 + aarch64 + riscv64），**不做**全量 machine IR。  
2. **共享的是「决策」不是「编码」**：输出仍是各 arch 可消费的中间描述或调用序列意图，不是机器码。  
3. **与 A 系列 / 1a 错峰**：单独 `refactor(semantic): …` PR；禁止混进纯移动。  
4. **有回归**：试点前后 golden hash（x86）不变或仅有意变更；交叉 L0 矩阵不新增红。

#### 12.2.1 推荐试点候选（按收益/风险）

| 优先级 | 语义 | 为何适合 | 共享形态（示意） |
|--------|------|----------|------------------|
| **P0** | **短路求值** `\|\|` `&&`（及条件表达式分支不求值） | 多 arch 各写跳转，易漏 side-effect 顺序 | `semantic::short_circuit_plan()` → 意图 BB/jump 列表，或各 arch 必须调用的 `eval_cond_once()` 协议 |
| **P0** | **复合赋值单次求值** `a[i()] += 1` | 文档/bug 史上出现过 lvalue 双求值类问题 | `semantic::compound_assign_plan(lvalue_path, rhs)` → 每 arch 按同一顺序：算地址 → 读 → 算 → 写 |
| **P1** | **序列点上的 volatile 读序**（若有现成 fixture） | 与 C23/SSA 修复线交叉，价值高但易碰 opt | 至少共享「读次数/顺序」断言 helper + 双 arch fixture |
| **P2** | 标量 `cast` 提升次序表 | 可做成纯数据表 | `cast_promotion_table()` 查表，codegen 只映射到寄存器宽度 |

**明确不做进试点**：ABI、TLS 模式、inline asm、向量、x87、VLA 栈分配——耦合指令与帧，应留在阶段 5 大设计。

#### 12.2.2 试点落点（建议路径）

```text
src/codegen/semantic/          # 或 src/ir/semantics_lower.rs（若更贴 IR）
  mod.rs
  short_circuit.rs             # 决策/计划 API，无 arch 寄存器
  compound_assign.rs
  README.md                    # 各 arch 必须调用的协议（顺序不变量）

src/codegen/x86_64/…           # 改为调用 semantic::，不再自拼跳转顺序
src/codegen/aarch64.rs         # 同
src/codegen/riscv64.rs         # 同
```

**接口形态二选一（试点选定一种即可）**：

| 形态 | 说明 | 优缺点 |
|------|------|--------|
| **A. 计划（plan）** | 返回 `Vec<SemOp>`（LoadLvalue/StoreRhs/BrFalse…），arch 再 lowering | 清晰；接线成本中 |
| **B. 协议 + 钩子** | `on_before_rhs`/`on_store` 回调，顺序由共享代码驱动 | 改动更小；不易测「完整计划」 |

**试点验收 DoD**：

- [ ] 至少 1 条共享语义在 **≥2 arch** 移出私有 `gen_*` 顺序拼装  
- [ ] 专项 fixture：side-effect 计数（或输出序列）在 x86 L2/运行 + 交叉 L0/CI L1  
- [ ] x86 golden `.text` 不变，或变更附「有意」说明  
- [ ] `check_feature_matrix` / parity **无新增红**  
- [ ] `semantic/README.md` 写明「各 arch 必须遵守的顺序不变量」  
- [ ] 失败不回滚整个阶段 2——试点可独立 revert  

#### 12.2.3 与阶段 5 的关系

| | §12.2 试点 | 阶段 5 machine IR |
|--|------------|-------------------|
| 范围 | 2–3 语义 | stmt/expr 大面积 |
| 目标 | 证明**能收敛** + 堵住高风险分叉 | 降 `gen_*` 重复 |
| 何时 | Phase 2 并行小线 | 独立 RFC，内核 P0 之后 |

试点失败不否定 v1.1；**试点成功是阶段 5 的 go/no-go 证据**。

### 12.3 阶段 5 前置条件（从 v1.1 「可选」改为「有条件」）

在开 machine IR RFC 前必须同时满足：

1. 阶段 1 打包/诊断已稳定（1a–1e 已合）  
2. §12.1 `check_feature_matrix` 连续绿 ≥ 一个发布周期  
3. §12.2 试点 DoD 全勾  
4. 内核 P0 不处于红灯排障周  

否则禁止启动阶段 5，避免「重构套重构」。

### 12.4 Tier-1 产品清单（防「全标 Tier-2 = 完成」）

阶段 0 输出 **`docs/backend_tier1.md`**（或矩阵附录），**产品视角**写死最小集，例如：

| 用途 | 必须 arch | 必须 Feature（示例，以探针+产品讨论为准） |
|------|-----------|-----------------------------------------------|
| 内核 `make CC=dcc`（现实） | **仅 x86_64** | CoreArith/Call、Tls、InlineAsm(Full)、CfiUnwind、StackProtect、Bitfield、VaList、Lto（按内核选项） |
| 对外宣称「6 架构交叉」 | 6 个 enabled arch | CoreArith、CoreCall、Float、L0 全绿 + 文档链接矩阵 |
| Lua / libiberty 级 | x86_64（默认） | Core* + Float + VaList + … |
| 并行 codegen / Coff | 仅文档写 x86 | 不得写进「全 arch」宣传 |

**阶段 4 DoD = 清单行全绿**，不是「矩阵无红」；矩阵可以 partial，**产品清单不允许长期红**（红则改宣传或改实现）。

### 12.5 三级验收（修正「做完是否一致」的表述）

| 级别 | 定义 | 何时算过 |
|------|------|----------|
| **L-A 度量一致** | 探针=文档=FeatureSet；失败语义统一；RV32 不说谎；parity L0 绿 | 阶段 0–2 结束 |
| **L-B 语义抽查一致** | §12.2 试点 DoD；关键 side-effect fixture 多 arch 过 | Phase 2 并行 + 持续 |
| **L-C 能力拉平** | §12.4 Tier-1 清单全绿 | 阶段 4 |

**宣传与验收禁止混用 L-A 当作 L-C。**

**结构回归附加（A 系列/阶段 1）**：

- x86 拆分后：10–20 golden `.text` hash  
- `-O2` 下 representative fixture 编译耗时 ±预算（如 10% 内，基线阶段 0 记）  
- `.text` 总体积漂移告警阈值  

### 12.6 明确 out of scope（本方案不声称解决）

| 领域 | 说明 |
|------|------|
| 内置/外部 **linker** 语义等价 | `link/` 已多架构，**未**纳入 Feature 探针主表；可另表 |
| **LTO** 跨 TU 质量与全 arch | 几乎绑定 x86 路径 |
| **sanitizer** 运行时全 arch | matrix 标 partial/Err 即可，不承诺拉平 |
| **C++** native / ABI | 独立 `cpp_native_backend_implementation_plan.md` |
| **性能打 GCC** | 与 `next_steps` kernel-bench 另线 |
| Darwin/WASM 等 | `missing_features` 已排除 |

### 12.7 角色与回潮问责

| 角色 | 职责 |
|------|------|
| 矩阵/CI owner | `check_feature_matrix` 红灯 24h 内修或 revert 引入 commit |
| 语义试点 owner | §12.2 DoD；顺序不变量文档 |
| 产品/Tier owner | §12.4 清单 vs README 宣传一致 |
| 拆分执行者 | A 系列只动结构；发现语义分叉 → **开 issue 进 §12.2 候选**，不在移动 PR 里修 |

### 12.8 v1.2 成功标准（并入 §1.3 的补充行）

| 标准 | 度量 |
|------|------|
| 防腐 | `check_feature_matrix` 在默认 PR 流必跑且失败会 block |
| 语义试点 | §12.2 DoD 全勾或显式 reject 并记录原因 |
| 产品一致 | Tier-1 清单与 README 无矛盾（阶段 0 初版、阶段 4 收紧） |
| 验收不掺水 | 对外/对内报告区分 L-A / L-B / L-C |

---

*方案版本：v1.2 — 防腐与语义单源试点已落盘；阶段 0–3 施工顺序保持不变。阶段 0 执行后回填：探针入口、`check_feature_matrix` 链接、Tier-1 初版、golden hash。*
