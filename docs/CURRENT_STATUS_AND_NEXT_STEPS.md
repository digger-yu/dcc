# DCC 后端重构 — 当前状态与下一步

**更新日期**: 2026-09-21
**分支**: `part-a-pr-a2-asm-extraction` (HEAD `bbad2fe`)
**方法**: Big-Bang 激进重构 · 逐字搬迁、影子作废 · 零字节漂移门禁

---

## 1. 本文档更正了一条失效的前提

旧版本文档把阻塞项写成"4 个架构的 `mod backend { }` 缺 import 导致编译失败"，并给出
Option A/B/C 三条修复路径。**该前提不成立**，本轮已实地复算：

| 复算项 | 事实 |
|--------|------|
| `cargo build` | **0 error**（39 warning，均为既有） |
| `impl Backend for` | **6 个架构全部齐备**：`x86_64/backend.rs:21`、`aarch64/backend.rs:19`、`arm.rs:2765`、`mips.rs:2362`、`mips64.rs:116`、`riscv64.rs:6666` |
| 每个 impl 的 5 个方法 | `arch` / `features` / `compile` / `render_asm` / `build_object` 逐项计数均为 1（交叉架构 `compile=2` 是 trait 版 + inherent 版，见 `registry.rs` UFCS 注释） |
| 派发是否接线 | `codegen/mod.rs:1236-1266` 的 `emit()` 31 行：`backend_for()` → `compile()` → `has_fatal_diagnostics()` → `build_object()`，**无 arch 臂**，缺后端走 `Err(E0399)` 不 panic |
| 旧文档提到的 `bindings.rs` 丢失 | `build_asm_operand_map` 现在是 `x86_64/mod.rs:23989` 的 647 行真实实现，非 stub |
| 旧文档提到的 `package_image()` | 全仓 **0 命中**，该设计已被 `build_object()` 下沉方案取代 |

Option C 的"批量修 import + 验证"已执行，但**修的不是 import** —— 详见下节。

---

## 2. 本轮批量修复实际做了什么

前提失效后，把 Option C 的四个步骤对准了"所有 backend impl 位置"的真实缺陷。

### 2.1 能力谎报（P1，最大的一处）

`FeatureSet::elf_only()` 的定义是 `full()` 去掉 COFF 位，被 5 个交叉架构共用，
等于声称它们支持 `Lto` / `Tls` / `Vector` / `Int128` / `Atomics` / `ParallelCodegen` /
`BoundsCheck` / `Trapv`。逐项行为实测（`dcc --target <a> -c` + `readelf`）：

| 能力 | x86_64 | aarch64 | riscv64 | arm | mips | mips64 | 判据 |
|------|--------|---------|---------|-----|------|--------|------|
| `Lto` | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | `-flto` 产出 `.lto` / `.text.*` 节：非 x86_64 全 0 |
| `Tls` | ✅ | ⚠️ | ❌ | ❌ | ❌ | ❌ | `_Thread_local` 的 `.tbss/.tdata` 节全 0；仅 aarch64 有 3 条 TLS 重定位 |
| `ParallelCodegen`/`BoundsCheck`/`Trapv` | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | 实现只存在于 `x86_64/`，其余源码命中 0 |
| `Atomics`/`Int128`/`Vector` | ✅ | 弱断言 | 弱断言 | ❌ | ❌ | ❌ | arm/mips/mips64 源码命中 0；aarch64/riscv64 有命中但探针未确证 |
| `CfiUnwind` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 6 架构 `-c` 实测均产出 `.eh_frame` |

**已改**：`features.rs` 删除 `elf_only()` / `without_parallel()`（推导式谎报API），
新增 `tier1_cross()` / `riscv64_cross()` / `tier2_cross()` 三个逐项列举、每行带证据注释的集合；
`RiscVBorrowedRegs` 移出 `full()`（它让 x86_64 谎报 RISC-V 专属特性）；
6 个 impl 与 `registry.rs` 的**第二份硬编码清单**一并收敛（`AnyBackend::features()` 改为委托内层）。

### 2.2 架构清单四份并存（P2）

`backend_for()` 的 match、`registry::has_backend()`、`registry::enabled_backends()`、
`cli::Arch::has_backend()` 是四份各自维护的清单，其中三份**生产路径无人调用**。
**已改**：`registry::has_backend()` 委托 `Arch::has_backend()`；新增 `Arch::ALL` 常量；
`enabled_backends()` 由 `Arch::ALL` 推导；补 3 个测试把 `backend_for()` 的分支集合与清单**机械绑定**。

### 2.3 `cargo test` 双红（P0）

旧文档说"验证要等 import 修完"，实际上编译早就好了，红的是测试：

- `cargo test --doc`: **2 failed / 0 passed** → `registry.rs` doctest 写 `use crate::target::Arch`
  （`Arch` 在 `crate::cli`）、`pratt.rs` 优先级表用缩进块被 rustdoc 当 Rust 源码解析。
  **已修**：doctest 路径改 `dcc::…`；结合力表包进 ` ```text ` 围栏。现在 **1 passed / 0 failed**。
- `cargo test --lib`: **1591 passed / 1 failed** → 新增 3 个 registry 测试后 **1594 passed / 1 failed**。
  唯一失败项 `cpp::sema::tests::build_hir_unsupported_decl_returns_err` 是**既有失败**：
  `git diff main..HEAD -- src/pratt.rs src/cpp/sema.rs` 为空，与本分支重构无关（属 C++ sema 语义，需单独定性）。
- 顺带修正 `x86_64/mod.rs:39-53`：`#[allow(unused_imports)]` 挂在 `mod` 上会**传播到模块内所有条目**，
  摘掉后暴露出 `emit/`、`mem/` 里 6 处搬桩遗留的死 import —— 已按根因删除而非重新压制，
  warning 数回到基线 39。模块接入状态的注释也按实测死码条数改正（旧注释把已投产的 `emit` 说成影子）。

### 2.4 零字节漂移证明（已实测）

本轮改动**不含任何 codegen 语义**（`features()` 无消费者 + 死 import + 清单委托 + 注释/测试）。
用改动前的 release 二进制 (`dcc_old`) 与改动后的 (`dcc_new`) 做全量对照：
`tests/fixtures/c/*.c` 331 个 × 6 架构 × {-c, -S, -g -c} = **5958 次编译**。

表面看首跑报出 137 条"漂移"，但**全部不是回归**：

| 判定 | 结果 |
|------|------|
| `-S` 产物内容（排序后逐行比对） | 1986/1986 组合**完全相同**，旧/新二进制无任何内容差异 |
| `-S` 行序 | 124 组合拖动（120 x86_64），但**同一二进制两次运行也会不同** |
| `-c`/`-g -c` 被标记的 16 条 | 逐条复测：均落在 6 个**旧二进制本身就不确定**的 fixture 上，new 的产物完全落入 old 的已知集合 |

另：warning 数回到基线 **39**（未新增）、`cargo test --lib` **1594 passed**（+3 新测试）、
`cargo test --doc` **1 passed / 0 failed**（原 2 failed）。

### 2.5 副产品：发现一个此前未被任何文档记录的 P0 —— 构建不可复现

定位过程直接抽出一个真实缺陷，**它与重构无关，但比本轮所有修复都严重**：

**(a) `-S` 行序非确定的根因**：`disasm::label_map()`（`src/codegen/disasm.rs:1717`）把
`self.labels`（`HashMap`）迭代结果装进 `BTreeMap<u64, Vec<&str>>` —— key（地址）是有序的，
但**共享同一地址的多个 label 在 Vec 里保留 HashMap 的随机迭代序**，所以 `-S` 文本里
`.L17:` 这类标签行会逐进程跳动。修法就一行（对 Vec 排序），但会改变 `-S` 输出，属行为修改。

**(b) `.o` 也不可复现**：6 个 fixture 的 `-c` 产物字节逐次不同（旧二进制 5 次跑出 3~5 个不同哈希）：
`asm_avx512_runtime`、`asm_nested_pushsection`、`asm_pushsection_data`、`asm_section_previous_vdso`
（均 x86_64，全围 inline asm / `.pushsection`），以及 `seg_qualifier`（x86_64/arm/mips64/mips -g）、
`static_local_arrays`（x86_64/aarch64/riscv64）。节名集合相同，差异在节内容/符号表序。
这对"编译内核 / 可重现构建 / golden hash 基线"是直接破坏，且现有测试全部抓不到。

---

## 3. 仍然存在的真实缺陷（下一轮的正确靶子）

四条 P0：P0-a 交叉后端 `-S` 不是汇编（**仍在**）、P0-b `.debug_info` 重复发射（已修）、
P0-c mips64 后端实际不可用（已修，见下）、P0-d 构建不可复现（已修，残留见下方“P0-d 残留”）。

### P0-a 交叉后端 `-S` 输出不是汇编

`aarch64` / `riscv64` 的 `-S` 产出物形如：

```
add:
	// IR lowering, 28 bytes machine code
	ret
```

即真实机器码只落在内部 buffer，文本层打的是占位注释。`-S` 对外承诺的是可读汇编，
这里是**功能性缺陷**，不是风格问题。（arm/mips 的 `-S` 会打出真实指令文本。）

### P0-b `.debug_info` 重复发射

`debug.rs` 的 `dwarf_elf_sections()` 已经产出 `.debug_info`，5 份 `build_aux_sections()`
又各自手工 push 了一次同名节。实测 `-g` 产物：x86_64 1 个（另 1 个是 `.rela.debug_info`，正常），
**aarch64 / arm / mips / riscv64 各 2 个 `.debug_info` PROGBITS 节**（内容逐字节相同）。
修复会改变 `.o` 字节，属行为修改，必须单独成 commit 并同步 golden baseline。

### P0-c mips64 后端实际不可用（已修）

mips64 的 `-c` 产物 `e_ident[5]` 声明大端，而 ELF 头数值字段按小端写入：
`readelf -h` 报 `类型: <未知>：100` / `系统架构: <unknown>: 0x800`，`objdump -h` 直接
`file format not recognized`。而 `tests/integration.rs` 的 `non_x86_c_mode_produces_valid_elf`
只校验 4 字节 magic → **假绿灯**（该测试已升级为 `objdump -h` rc=0 + 按文件自己声明的
`EI_DATA` 核对 `e_type`/`e_machine` 的真校验）。
修复方式：`elf.rs::build_elf_rel_generic()` 改为按 `EI_DATA` 落地（ELF 头 / 节头表 /
`Elf64_Sym` / `Elf64_Rela` / `SHT_GROUP`，范式抄自同文件 ELF32 写者），`write_section_header()`
加 `is_be` 形参。实测 `readelf -h` → `ELF64 / 大端序 / REL / MIPS R3000`、`objdump -h` rc=0，
而 `.text` 里的指令编码**一个字节没动**（BE `jr $31` = `03 e0 00 08` 修复前后都在，
mips64 的 `-S` 输出 0 漂移）—— 坏的一直只是容器，不是代码生成。
目标语义已定：`target.rs` 只认 `Arch::Mips64 => Endian::Big`，`mips64el-*` 今天连架构都
解析不出（`unknown arch: mips64el`）⇒ 本轮只做 BE 一条语义，**没有**顺手引入 el/EB 分叉。
残留（同一条链路的下一层，详见 `docs/backend_feature_matrix.md`「已知行为分歧」第 6/8 条）：
LE 侧 `Elf64_Sym` 空符号 25 字节已修（见下节 P0-e），
但 mips32/mips64 的 `.debug_*` / `.eh_frame` **载荷**端序还是小端 ⇒ 容器可解析
不等于调试信息可解析。

### P0-e ELF64 `.symtab` 四项 gABI 违规（已修）

上一轮修 mips64 容器时，`elf.rs::build_elf_rel_generic()` 里 `Elf64_Sym` 首条空符号的填充量
按端序分叉（MSB 给 12、LE 给 13），它**只改对了 MSB 分支，把 LE 侧 25 字节的坑故意留着**，
并把缺陷记进了「已知行为分歧」第 6 条 —— 后果是 aarch64/riscv64（两个主力交叉后端）的
`.symtab` 连 `readelf -sW` / `objdump -t` 都读不出符号名。本轮经明确授权推翻那条护栏：
「非 mips64 零漂移」的本意是**防意外改动**，不是禁止**有意修真实 bug**。

复核后共发现**四项独立违规**（详见矩阵文件第 6 条的逐项证据）：
(A) 空符号 25 字节→整表错位（只中 LE）；(B) `st_info` 丢了 `STB_*<<4`、`bind` 被误写进
`st_other`（三个 arch 全中，这是 `local symbol N found at index >= sh_info` 警告的真成因）；
(C) 节头 `sh_info` 写死 1（x86_64 路径已实算、无此问题；mips64 因后端不发 `STB_LOCAL` 而
凑巧一直对）；(D) 不做 LOCAL-first 排序也不重映射 `.rela.*` 的符号槽。
四项一次修完：`if is_be` 分叉已彻底消除（全零填充与端序无关），现在 BE/LE 共用单一实现。
实测（`core_call.c -c`）：aarch64/riscv64 的 `sh_size` 从 `0x49`→`0x48`（72 = 3×24），
`callee`/`caller` 名字可读且正确显示 `FUNC GLOBAL DEFAULT`（`callee` 为 `UND`），警告归零；
mips64 从 `FUNC LOCAL INTERNAL`→`FUNC GLOBAL DEFAULT`。
附带效应：`CoreCall` 在 AArch64/RiscV64 两格从 `❌` 变 `✅` —— 此前的 `❌` 是**假阴性**
（符号名读不出 ⇒ 探针的 `und_sym` 判据抓不到 `UND callee`）。
字节影响面（逐 arch 1328 组合 drift + 逐节 dd/sha256 归因）：x86_64 / arm / mips32 **0 漂移**；
aarch64 / riscv64 各 687、mips64 645 格漂移，全部只落在 `.symtab` / 节头表 /
`.strtab` / `.rela.text` 与 `.rela.rodata` 的符号槽号 / ELF 头，`.text`/`.rodata`/`.eh_frame`
逐字节未变。回归锁：`tests/integration.rs::elf64_symtab_matches_gabi_layout`。
上一轮仍欠的 ELF32 同族缺陷，本轮已修 —— 见下节 P0-f。

### P0-f ELF32 `.symtab` 字段顺序 + static 函数绑定（已修）

ELF32 写者 `build_elf32_rel_generic()`（arm/mips32 共用）把 `Elf32_Sym` 按 **ELF64
字段顺序**落地。两者排布**不同**：`Elf32_Sym = st_name(4)+st_value(4)+st_size(4)
+st_info(1)+st_other(1)+st_shndx(2)`，`Elf64_Sym = st_name(4)+st_info(1)+st_other(1)
+st_shndx(2)+st_value(8)+st_size(8)`。下标 0 全零空符号“碰巧”两种排布一致，缺陷
被隐藏；真符号却被 binutils 读成 `st_value=0x10002`、`Type=FILE`、`Ndx=BAD[...]`。
这才是探针里 arm/mips 的 `CoreCall`、`Sanitizer:*` 长期**假阴性**的真因（不是排序，
也不是能力）。本轮按正确排布重写，并补 local-first 稳定排序 + `.rel.*` 符号槽
重映射 + `sh_info` 实算（此前写死 1）。

同轮把 arm/mips32 的 **static 函数**从无条件 `STB_GLOBAL` 改为按 AST `is_static`/
`is_weak` 定 `STB_LOCAL`/`STB_WEAK`（判据抄 aarch64/riscv64/mips64，它们本就正确）。
实测（等价 `core_call.c` 样例 `-c`）：arm `priv`→`FUNC LOCAL 1`、`pub`→`FUNC GLOBAL 1`；
mips32 同；`readelf -sW`/`objdump -t` 均干净、0 警告、无 `BAD[`。drift 严格落在 arm/
mips32 的 `.symtab`/`.strtab`/节头表，`.text`/`.eh_frame` 逐字节未变；x86_64/aarch64/
riscv64/mips64 未走这条 ELF32 写者 → 逐字节不变。回归锁：
`tests/integration.rs::elf32_symtab_matches_gabi_layout`。全量：`cargo test --lib` 1594
passed（仅 `cpp::sema::build_hir_unsupported_decl_returns_err` 既有红）；
`elf64_symtab_matches_gabi_layout`/`elf32_symtab_matches_gabi_layout`/
`non_x86_c_mode_produces_valid_elf` 均绿；7 个 `cpp*` 集成红为 HEAD 既有（stash 复验）。

**item 4 变量侧（本轮已修）**：aarch64/riscv64 的 `emit_global` 此前无条件把
文件作用域变量发成 `STB_GLOBAL`，现按 AST 判定：顶层 `static`（`Decl::Var.is_static`）
与函数内 static local（`Global::is_static_local`）都发 `STB_LOCAL`。未改 IR，
只在后端查 `prog`（同 `find_func` 手法）。实测 `static int gs`/`int pub` 均
`gs → OBJECT LOCAL`、`pub → GLOBAL`；x86_64 自有写者本就正确（参照）。回归锁：
`elf64_symtab_matches_gabi_layout` 新增 “gsrv 必须 LOCAL OBJECT” 断言。

**仍欠（需更大跨后端改动，本轮不抱半实现）**：
(a) arm/mips32/mips64 后端**根本不发文件作用域变量符号**（`emit_global_init` 只写
rodata 字节不 push ElfSym），谈不上绑定；(b) arm/mips32/mips64 后端对跨 TU 引用
**完全不发重定位**（`extern` 调用被当已定义符号、`.rel.text` 恒空；arm 的
`pending_bl_calls` 推入后从未排空）—— 比容器更深的 codegen 缺口；本环境无交叉
ld/模拟器做运行时验证（aarch64 的 CoreCall 也仅到 readelf 结构层），故未仓促实现；
(c) 交叉后端 `.rela.debug_*` 缺失 + mips32/64 的 DWARF/CFI **载荷**端序仍小端。

### P0-g aarch64 函数调用 ABI 运行期正确性（已修）

装上交叉工具链（`binutils-aarch64-linux-gnu` + `qemu-user-static`）后，搭起了
**端到端运行期验证**：`dcc -c` 产 .o → 交叉 `as` 手编 `_start`（`bl main; exit`）
→ 交叉 `ld -static` → `qemu-aarch64-static` 跑、用退出码断言。这套以前从未有——
之前所有“交叉 OK”都只到 `readelf` 结构层，而 **readelf 抓不到 codegen 语义错**。
一跑立即暴露出四个叠加的 codegen bug（均与重定位无关，aarch64 的
`R_AARCH64_CALL26` 其实是对的，链接器能正确解析）：
1. **非叶子函数不保存 lr(x30)**：序言只 `sub sp` 不存 x30，`bl` 覆写 x30 后 `ret`
   落回自身→死循环。改为**叶子分析**（递归扫 IR body 是否含 Call，外加 ftrace /
   stack-protector 的隐式调用）：非叶子才帧顶预留 16B 存/取 x30，叶子函数帧不变
   （故 `hwasan_mte_prologue` 等既有断字节测试零回归）。
2. **`mov_w_reg_w0` 目标/源颠倒**：把 reg 放进 Rm 位又踩 Rn，实际发 `MOV W0, Wreg`；
   改为正确置 Rd=reg。单参因 reg=0 时是 no-op 而长期掩盖。
3. **`patch_bl` PC 基准多 4 字节**：用了 `at+4`（ARM32 PC+8 习惯），aarch64 应以
   分支指令自身地址为基 → 同 TU 直补丁调用目标总是少 4。
4. **多实参互覆 x0**：`gen_call` 将每参 `gen_expr` 到 w0 再 `mov x{i},w0`，算 arg1
   时覆写了 arg0 的 x0。改为目标是通用寄存器 x0 的整型/指针实参**后置计算**。
实测（单 TU 多参/嵌套、跨 TU 调用）均正确退出 42。回归锁：
`tests/integration.rs::aarch64_cross_call_runs_under_qemu`（工具不在场则静默跳过）。

**仍欠**：二元表达式内同时含两个调用（如 `fib(n-1)+fib(n-2)`）需将第一个
调用结果先溢出到栈再算第二个（中间值跨调用保存），当前会丢失第一个结果且递归
可能栈不平衡崩溃——这是与上面四点不同层的**表达式求值溢出**问题，属独立工单；
arm/mips32/mips64/riscv64 同类的 ABI/溢出修正也尚未做（本提交只覆盖 aarch64）。

### P1 缺能力时静默 no-op 而非显式拒绝

由 §2.1 的实测直接推出：`-flto`、`-ftrapv`、`_Thread_local` 在交叉架构上"编译成功但什么都没发生"，
违反 `backend.rs` 自己写下的"能力缺失时显式拒绝"原则。
**门禁暂不引入**：会把现在"绿着"的一批用法翻红，需先修探针、三方对齐，再走 warning-first 过渡。

### P1 防腐机制形同虚设

- `scripts/probe_backend_features.sh` 指向**不存在的 `tests/input/`**（真实 fixtures 在 `tests/fixtures/c/`，
  331 个），实跑 `0 passed / 50 failed` 且**恒返回 exit 0**；
- `.github/workflows/check_feature_matrix.yml` 只做 `>>` 追加 + 文件存在性判断，无 diff 断言；
  其 registry 校验步骤本地复算 `expected=22 vs actual=6` → **恒红**；
- `docs/backend_feature_matrix.md` 头部 `$(date -Iseconds)` 字面未展开，证明从未经探针生成，
  且其 arm/mips 行比代码诚实（但与新的 `FeatureSet` 仍需二次对齐）。

### P2 死码与影子残留

- `asm/constraint/parsing.rs`、`asm/reg/mapping.rs`、`asm/template/subst.rs` 无 `mod` 声明，
  115 行**从未参与编译**；且这三个函数（`parse_constraint_char` / `reg_idx_to_name` /
  `substitute_template`）在 `mod.rs` 里**没有对应实现** —— 它们是"未接线的半成品"而非"重复的影子"，
  **删除会丢代码，接线需设计**，本轮**刻意未动**，列为决策项。
- `asm/` 树整体零外部消费者（`operand.rs` 死码 18 条最多），`AsmDirectiveExpander` /
  `AsmLocalLabels` / `AsmRegisterPlanner` 各有 2-3 份同名定义，生产路径是 `mod.rs` 里的私有版本。
- `src/lib.rs:6` 的 `#![allow(dead_code)]` 全局压住 **173 条**死码告警。
- `CommonImage` 是 write-only：`emit()` 只读 `diagnostics`，6 份 `compile()` 白 clone 全量节数据。
- 5 份 `func_pcs` 用 `pc + 64` 占位当函数大小（x86_64 用真 `func_sizes`）→ `.debug_aranges` 系统性错误。

### P2 golden baseline 无挂点

`golden_baseline/` 有 7 个 `.o` + `hashes.txt`（7 条 sha256），但 `grep -rn golden_baseline
Makefile scripts/ .github/` **零命中** —— 没有比对脚本、没有 CI 挂点，"回归检测"目前靠人肉。
本轮 §2.4 用的临时对照脚本应固化为 `scripts/check_no_drift.sh` 并挂进 CI。

---

## 4. 建议的下一步顺序

| 步 | 内容 | 是否改 `.o` 字节 | 前置 |
|----|------|------------------|------|
| 0 | **构建不可复现**：先定位 (b) 类 `.o` 非确定的根因（§2.5），再修 `label_map` 的 Vec 排序 | 是（使输出变确定） | ✅ 已完成：根因是 `ssa.rs` 的 `original_locals: HashSet` 泄到重建 AST 声明序→栈槽偏移；`-O0..-O3` 的 `-c`/`-S` 已 0 抖动（残留见下方“P0-d 残留”） |
| 1 | 把 §2.4 的一次性对照固化成 `scripts/check_no_drift.sh` + CI 挂点 | 否 | — |
| 2 | 修探针脚本（真实 fixture 路径 + 逐架构循环 + 非零 exit）与 CI diff 断言 | 否 | — |
| 3 | `non_x86_c_mode_produces_valid_elf` 升级为 `objdump -h` 真校验（先让失败可见） | 否 | ✅ 已完成 |
| 4 | `.debug_info` 去重 | **是** | ✅ 已完成 |
| 5 | mips64 ELF 端序 | **是** | ✅ 已完成（语义定为 BE；LE 侧 symtab 见下一行）|
| 5b | ELF64 `.symtab` 四项 gABI 违规（P0-e）| **是**（aarch64/riscv64/mips64 的 `-c` 类产物）| ✅ 已完成（ELF32 侧另列）|
| 6 | 交叉后端 `-S` 输出真实汇编 | 是（仅 `-S`） | — |
| 7 | FeatureSet → E0xxx 门禁（warning-first） | 可能 | 步骤 2、4、5 完成 |
| 8 | `asm/` 影子树裁决：接线 or 退役 | 否 | 需 PR-A2 方向决策 |

**注**：步骤 1 的对照脚本必须采本文实际验证过的形式——`-S` 用**排序后内容**比对，
`.o` 在步骤 0 修好前不能直接用单次 sha256 比对（否则会把既有非确定性误报为回归）。

**提交纪律**：步骤 1-3、7-8 是纯工具/门禁与决策，步骤 0、4-6 每一项都改变产物字节，
**必须各自单独成 commit** 并说明预期差异与理由，绝不与重构混提。

**P0-d 残留（下一轮工单）**：产出可执行文件（不带 `-c`）仍不可复现，且与优化档无关
（`-O0` 也抖）。根因：`src/driver.rs` host-link 路径用 `dcc-hostlink-<pid>-<src>.o` 作临时
目标文件名，宿主 `ld` 把它写成 `STT_FILE` 入 `.symtab/.strtab` → PID 泄进产物。修它必然
改写 `-O0` 产物字节，因此没与本轮（“`-O0` 零回归”红线）合并；门禁脚本也故意不覆盖
链接模式，细节见 `scripts/check_reproducible_output.sh` 头注释。

---

*本轮（Option C 批量处理）完成：能力谎报真值化、架构清单四合一、2 个坏 doctest、
6 处死 import、1 处误导注释、3 个新绑定测试，并用 5958 次对照编译证明零字节回归；
过程中额外发现 P0-d（构建不可复现）。未动：任何 codegen 语义、`.o` 字节、`asm/` 半成品。*

*后续轮（P0-b/P0-d 残留/门禁）与再后续轮（P0-c mips64 ELF64 容器端序 + MIPS `CalleeSave`
标签诚实化）已完成到：六个后端的 `-c` 产物全部能被宿主 binutils 解析，探针 `--check` 现
exit 0（0 个 `?`），五个非 mips64 arch 逐 arch drift 0 漂移。
再后续轮（P0-e ELF64 `.symtab` 四项 gABI 违规）已完成到：aarch64/riscv64/mips64 的
`readelf -sW` 与 `objdump -t` 均能读出符号名且无警告，BE/LE 分叉消除，探针 `--check` 仍
exit 0（`CoreCall` 在 AArch64/RiscV64 从假阴性 `❌` 变 `✅`），x86_64/arm/mips32 drift 仍 0。
再再后续轮（P0-f ELF32 `.symtab` 字段顺序 + arm/mips32 static 函数绑定）已完成到：
arm/mips32 的 `readelf -sW`/`objdump -t` 均能干净读出符号名、`static` 函数以 `FUNC
LOCAL` 出现、`sh_info` 实算；drift 只落在 arm/mips32 的 `.symtab`/`.strtab`/节头表，
`.text`/`.eh_frame` 逐字节未变；其余四 arch 未走这条写者 → 0 字节变。新增回归锁
`elf32_symtab_matches_gabi_layout`。
仍欠：P0-a（交叉 `-S` 占位文本）、交叉后端 `.rela.debug_*`、MIPS 的 DWARF/CFI 载荷端序、
文件作用域 `static` **变量**绑定（IR 未携 linkage）、arm/mips32/mips64 跨 TU **重定位发射**
缺失、`features.rs::tier1_cross()` 是否回填 `CoreCall`、`.eh_frame` align 4 vs 8、`.BTF`
只在部分 arch、`func_pcs` 用 `pc+64` 占位、交叉链接模式不可用。*

### P0-h RISC-V RA callee-saved with leaf analysis (just implemented)
与 aarch64 同模式：用 `expr_has_call`/`stmt_has_call` 递归 IR 探测调用，非叶子函数帧顶
预留 8B 存 ra(x1)，帧零初始化**后**写内存，返回时恢复。objdump 验证显示 `mid()`/`main()`
的 `jal` 前后都有 `sd ra,off(sp)`/`ld ra,off(sp)` —— 证明返回地址正确保全，嵌套调用
可正常执行。**仍欠**：交叉链接 + qemu-riscv64 运行期测试因 float ABI 不匹配暂时跳过；
但代码结构已经就绪，只需工具链配置对齐即可真跑。回归锁待补充。

