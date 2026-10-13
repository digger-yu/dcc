# AArch64 内联汇编编码缺陷登记

来源: `.trae/documents/verified-code-gaps-and-stale-claims.md` A.5 / A.13 / A.14。
本文只登记现象与位置, 不在此擅自修改编码行为; 每条给出触发条件与当前为何未暴露。
行号以符号名为准, 漂移后按符号检索。

## A.5 mini_asm 编码存疑 (部分修复于 G11)

位置: `src/codegen/shared/mini_asm.rs`

现象:

1. `ld1` (mini_asm.rs:530-536) 注释基址写 `0x4C007400`, 代码实际用
   `0x4c407400`; `st1` (mini_asm.rs:539-545) 注释 `0x4C006400` 与代码一致。
   同族 128-bit load/store 的基址立即数只应在 load/store 位上不同,
   `0x4c407400` 与 `0x4c006400` 之间必有一处基址错误 (注释与代码不一致
   本身即说明无人对照手册核过)。
2. SVE 四条臂自标 `(simplified)`: `fadd` (mini_asm.rs:571-581,
   `0x65000400`)、`add` (mini_asm.rs:584-594, `0x45000400`)、
   `ld1w` (mini_asm.rs:597-603, `0x85800000`)、`st1w`
   (mini_asm.rs:606-612, `0xe5800000`)。缺 predicate 后缀 (Pg) 与
   长度/元素选择字段, 按寄存器号低 5 位直接拼接, 编码很可能仅对特定
   形态碰巧正确。
3. `whilelt` (mini_asm.rs:558-568, `0x2528a400`) 与上面四条同型
   (按 pd/rn/rm 低位拼接, 无 predicate 语境), 却**未标** simplified —
   标签与 fadd/add/ld1w/st1w 不一致, 同型存疑被漏标。

G11 修复进展:

- **ld1/st1 已由 objdump 真值确证并修正**: ld1 v0,[x1] 与 st1 v3,[x5]
  经 `aarch64-linux-gnu-as` + `objdump -d` 对照, LD1 基址应为
  `0x4C407000` (bit12 length=1 单结构, 旧代码 `0x4c407400` 等价, 仅清
  注释对齐); ST1 基址应为 `0x4C007000` (旧代码 `0x4c006400` 把 bit12
  错清 0, 是真错误)。单测期望值同步更新, 冻结基线 golden 重新生成
  (aarch64 DIGEST: `846508badb538db1` → `c89eef41b2a64e29`)。
- **SVE 五条 (whilelt/fadd/add/ld1w/st1w) 未修**: 基址骨架缺
  size[31:30]/Pg[11:10]/M[9] 字段, 与 objdump 差距大, 属"简化子集硬
  编码"。按红线约束, 不在待删除的 match 链上继续堆操作数解析; 走
  P1-1/P1-2/P1-3 统一管线落地后再重写。objdump 对照 (2026-10-14,
  `-march=armv8-a+sve`):

  | 助记符 (输入) | mini_asm 当前输出 | objdump 真值 | 差异 |
  |---|---|---|---|
  | whilelt p0.b, x0, x1 | `0x2528a420` | `0x25211400` | 骨架完全错 |
  | fadd z0.s, z1.s, z2.s | `0x65020020` | `0x65820020` | bit22 size 字段错 |
  | add z0.s, z1.s, z2.s | `0x45020020` | `0x04a20020` | 骨架完全错 |
  | ld1w z0.s, p0/z, [x1] | `0x85800020` | `0xa540a020` | 骨架完全错 |
  | st1w z0.s, p0, [x1] | `0xe5800020` | `0xe540e020` | 骨架完全错 |

  结论: 这 5 条 SVE 内联汇编**产出的字节无法被真实 CPU 正确译码**, 但内
  联汇编路径要求用户手写 SVE 形态, 现有 C 代码不触发; 不接 P1 统一管线
  前不在 match 链上继续修。

触发条件: 内联汇编写出 `ld1 v0.16b, x1`、SVE `fadd z0.s, z1.s, z2.s`
等对应形态即命中, 产字节但语义可能错。

为何未暴露: 无 runtime 执行验证; 输出被冻结基线
`encode_freeze_is_byte_identical` (mini_asm.rs:1352, golden:
`tests/fixtures/asm/mini_asm_encode_freeze.txt`) 按现状逐字节钉死,
错误字节与正确字节同样稳定, 测试永远绿。

处置: 本次不改编码。修法需对照 ARM 架构参考手册逐条重算基址, 并配合
A.14 的 runtime objdump 复核; 改编码行为属有意变更, 需显式重新生成
冻结基线并人工审阅 diff (mini_asm.rs:1365 `regenerate_encode_freeze`)。

## A.13 patch_adr 越界退化已发生失效 (已修复于 G11)

位置:

- 入队: `src/codegen/aarch64/emit/expr/ops.rs:35-41` (`load_global_addr_w0`
  非 relocatable 分支)
- drain: `src/codegen/aarch64/codegen/pipeline.rs:591-594`
  (`finalize_exec` 逐条调 `patch_adr(at, rd, target)`)
- 失效点: `src/codegen/aarch64/emit/expr/patch.rs:57-91` (`patch_adr`)

G11 修复: 入队侧 `ops.rs` 改为统一调用 `emit_adr_large(0, target)`
预留 8 字节 (ADRP 占位 + ADD rd,rd,#0 占位), `patch_adr` 重写为双 slot
语义: 越界时原地填 ADRP+ADD 完整双指令; 未越界时填 ADR 并把 at+4
占位清 NOP。死码 `emit_adr_large` 接入生产路径, 页内偏移 `lo_off`
不再计算即丢。

触发条件: 非 relocatable (ET_EXEC / 直接产物) 模式下, 被引用的 rodata
全局常量距代码段 >1MiB。

为何未暴露: 测试走 relocatable 路径 — 该路径 (ops.rs:24-34) 发出
`emit_adr_placeholder(0)` + `R_AARCH64_ADR_PREL_LO21` 重定位, 由链接器
回填, 不经过 `patch_adr`; 既有测试地址模型都很小, 无 >1MiB 距离用例。

## A.14 PC 基准三种写法矛盾 (已修复于 G11, objdump 实证)

位置 (同一 `at: u64` 参数, 三种 pc 基准):

| pc 基准 | 符号 | 位置 | objdump 实证结论 |
| --- | --- | --- | --- |
| `at` | `patch_bl` | patch.rs:12 | 正确 (AArch64 硬件基准) |
| `at` | `patch_adr` | patch.rs:62 | 正确 |
| `at + 4` → `at` | `patch_b26` | prologue.rs:119 | **原 at+4 错, 已改为 at** |
| `at + 4` → `at` | `patch_cbz` | prologue.rs:130 | **原 at+4 错, 已改为 at** |
| `at + 4` → `at` | `patch_bcond` | cache.rs:118 | **原 at+4 错, 已改为 at** |
| `at + 4` → `at` | `patch_cbnz` | atomic_insn.rs:107 | **原 at+4 错, 已改为 at** |
| `at + 8` | `patch_ldr_u32` | patch.rs:43 | 正确 (ldr literal 后紧跟 b +8 双 slot, 独立理由) |

objdump 实证 (2026-10-14):
```
.org 0x1000
b  target      ; at=0x1000 target=0x1018 → 14000006, imm26=6 = (0x18)>>2 = (target-at)>>2
bl target      ; at=0x1004 target=0x1018 → 94000005, imm26=5 = (0x14)>>2 = (target-at)>>2
cbz x0,target  ; at=0x1008 target=0x1018 → b4000080, imm19=4 = (0x10)>>2 = (target-at)>>2
```
GNU as 产出的机器码证明 AArch64 所有 PC-relative 分支均以**指令自身地址
`at`** 为基准, 无 ARM32 的 PC+8 流水线偏移。原 at+4 写法把 imm 多减了 1,
导致 ET_EXEC 模式下分支目标偏前 4 字节。

**为何此前未暴露**: dcc 默认产出 ET_REL (relocatable) 目标文件, branch 指
令的 imm 占位由 R_AARCH64_JUMP26 / R_AARCH64_CONDBR19 重定位项链接器回填,
**不经过 dcc 自己的 patch_b26/patch_cbz PC 基准计算**; 只有 ET_EXEC 直产
物模式才走 dcc 自己的 patch, 而该模式无测试覆盖。

修复: `patch_b26/patch_cbz/patch_bcond/patch_cbnz` 的 `pc = at + 4` 统一改
为 `pc = at`。`patch_bl/patch_adr/patch_ldr_u32` 基准本就正确, 未动。

为何未暴露: 单测只断言机器码字段非空 / 冻结快照, 没有 runtime 执行
跳转验证。

**处置: 不写成定论, 不擅自改任意一组** (改错即把可用通路改坏)。需一次
runtime objdump 复核才能收口: 编译一个带 if/else 与 for 循环的小函数,
反汇编确认 B/BCOND/CBZ 的目标地址, 再定哪一组基准错误。

## 后续统一管线的 P1/P2 硬前置 (来自方案 A.1–A.3, 本次不实现)

当前 mini_asm 编码签名 `(mnemonic, &[String]) -> Option<Vec<u8>>`
(mini_asm.rs:1139 附近的 aarch64 match 表即此签名) 是 A.5/A.13 之外
一切"表内没有的形态只能透传"的根因。统一管线落地前以下为硬前置:

- **P1-1 内存操作数**: 编码函数的输入 (Operand) 必须能表达内存操作数
  及三种括号语法 — aarch64 `[x0, #8]`、arm `off(rs)`、mips
  `off($r)`。当前 aarch64 名单 29 个助记符无访存类, 是签名限制而非
  设计选择; 实测一切带 `{}` / `[]` 的 GAS 内联文本全部透传 (见护栏
  `gas_syntax_beyond_table_passthroughs`,
  src/codegen/shared/asm_source.rs)。
- **P1-2 Fixup**: 返回值必须带 `Vec<Fixup>` (符号引用 / 标签引用的重
  定位项), 不能直接产出最终字节; 否则跨 TU 符号只能继续透传给 GAS。
- **P1-3 Label 引用与松弛修补**: 短分支到长分支的长度升级
  (relaxation) 要有表达位。
- **P2 名单与表同源生成**: `known_mnemonics` (asm_source.rs) 与四张
  match 表目前手工同步, 已发生一次 "ld"/"st" 错列 (源码子串误命中
  dmb/dsb 的 domain 选项值 0xd/0xe)。需 derive / macro 同源生成,
  避免名单与表再次漂移。

红线: 统一管线落地前, **不得**在待删除的 match 链上继续堆操作数解析
(例如给现有 `ld1 v0, x1` 臂追加 `[x0]` 分支)。新增能力一律走新管线;
护栏 `mnemonic_table_matches_mini_asm_dispatch` 已从源码子串匹配改为
行为判据 (合法样本 analyze() 必须产字节, 名单外必须 UnknownMnemonic
透传), 可防此类假绿回归。
