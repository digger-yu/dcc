# AArch64 内联汇编编码缺陷登记

来源: `.trae/documents/verified-code-gaps-and-stale-claims.md` A.5 / A.13 / A.14。
本文只登记现象与位置, 不在此擅自修改编码行为; 每条给出触发条件与当前为何未暴露。
行号以符号名为准, 漂移后按符号检索。

## A.5 mini_asm 编码存疑 (未暴露)

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

触发条件: 内联汇编写出 `ld1 v0.16b, x1`、SVE `fadd z0.s, z1.s, z2.s`
等对应形态即命中, 产字节但语义可能错。

为何未暴露: 无 runtime 执行验证; 输出被冻结基线
`encode_freeze_is_byte_identical` (mini_asm.rs:1352, golden:
`tests/fixtures/asm/mini_asm_encode_freeze.txt`) 按现状逐字节钉死,
错误字节与正确字节同样稳定, 测试永远绿。

处置: 本次不改编码。修法需对照 ARM 架构参考手册逐条重算基址, 并配合
A.14 的 runtime objdump 复核; 改编码行为属有意变更, 需显式重新生成
冻结基线并人工审阅 diff (mini_asm.rs:1365 `regenerate_encode_freeze`)。

## A.13 patch_adr 越界退化已发生失效 (未暴露)

位置:

- 入队: `src/codegen/aarch64/emit/expr/ops.rs:35-41` (`load_global_addr_w0`
  非 relocatable 分支, 只占 1 个 slot: `emit_adr_placeholder(0)` +
  `pending_adr_exec.push((pos, 0, target))`)
- drain: `src/codegen/aarch64/codegen/pipeline.rs:591-594`
  (`finalize_exec` 逐条调 `patch_adr(at, rd, target)`)
- 失效点: `src/codegen/aarch64/emit/expr/patch.rs:57-86` (`patch_adr`)

现象: 当 `target - pc` 超出 ADR 的 ±1MiB 范围时, 越界分支
(patch.rs:60-78) 只把原 slot 原地改写为 ADRP 即 `return`。页内偏移
`_lo_off` (patch.rs:64) 算了即丢; 原地 patch 无法插入 ADD 指令字节
(注释 patch.rs:73-76 已自认)。结果: rd 得到的是页基址而非目标地址,
低端 12 位丢失 → 引用该全局常量的访存全部错位。

正确修法已存在但是死码: `emit_adr_large` (patch.rs:90-99) 按 ADRP+ADD
双 slot 占位并入队, 全仓零调用 (grep 仅命中 patch.rs:75 的注释与函数
定义本身)。

触发条件: 非 relocatable (ET_EXEC / 直接产物) 模式下, 被引用的 rodata
全局常量距代码段 >1MiB。

为何未暴露: 测试走 relocatable 路径 — 该路径 (ops.rs:24-34) 发出
`emit_adr_placeholder(0)` + `R_AARCH64_ADR_PREL_LO21` 重定位, 由链接器
回填, 不经过 `patch_adr`; 既有测试地址模型都很小, 无 >1MiB 距离用例。

## A.14 PC 基准三种写法矛盾 (不写成定论)

位置 (同一 `at: u64` 参数, 三种 pc 基准):

| pc 基准 | 符号 | 位置 |
| --- | --- | --- |
| `at` | `patch_bl` | patch.rs:12 |
| `at` | `patch_adr` | patch.rs:58 |
| `at + 4` | `patch_b26` | prologue.rs:118 |
| `at + 4` | `patch_cbz` | prologue.rs:128 |
| `at + 4` | `patch_bcond` | cache.rs:117 |
| `at + 4` | `patch_cbnz` | atomic_insn.rs:106 |
| `at + 8` | `patch_ldr_u32` | patch.rs:43 |

现象: AArch64 所有 PC-relative 分支均以指令自身地址为基准 (无流水线
偏移; patch.rs:9-11 注释自述 "at+4 是 ARM32 的 PC+8 流水线偏移")。
`at` 与 `at+4` 两组在同一 ISA 下不可能都对 — 若 `at` 正确, 则
b26/cbz/bcond/cbnz 四条主通路全部把目标算偏后 4 字节; 反之则
patch_bl/patch_adr 偏前 4 字节。`patch_ldr_u32` 的 `at+8` 有独立理由
(ldr literal 后紧跟 `b +8` 两条 slot 布局), 不参与矛盾。

调用面: `self.b(` / `emit_bcond(` / `emit_cbnz(` 合计约 64 处调用点
(6 个文件)。若整组都错, aarch64 分支将完全不可用, 与编译器能产出可
运行程序矛盾 — 说明至少有一组约定是对的, 但静态读码无法收口是哪组。

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
