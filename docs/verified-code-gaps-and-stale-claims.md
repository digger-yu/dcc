# 已核实的缺口与过期口径

状态：未开工 ｜ HEAD `1f7aa1c` ｜ 2026-10-09 ｜ 只分析，不改代码。
A 部分是交叉后端内联 asm 的能力缺口与发射/回填欠账；B 部分是 P12 落地后仍写旧世界的 6 处文字。
所有数字与行号均为实测，可用文末命令复取。

## A. 交叉后端内联 asm

对象：`src/codegen/shared/mini_asm.rs` 四张表（aarch64 / riscv64 / arm / mips）及调用方
`asm_source.rs`。实施归 `dcc-asm-unification-plan.md`，本文只记缺口与错账。

### 缺的能力

1. **内存操作数**：四张表 0 处解析 `[x0, #8]` / `off(rs)` / `off($r)`；
   `classify_line`（`asm_source.rs:253-263`）只按逗号切分 + trim，不剥 `[]` `{}`，token 原样进表。
   `ld1 {v0.16b}, [x0]` → `is_neon_reg`（`mini_asm.rs:38-46`）会剥 `.16b` 但**不剥花括号** → 假 →
   `None` → 透传。
   aarch64 表里那 4 条向量访存（`ld1:527` `st1:536` `ld1w:594` `st1w:603`）因此**不可达**。
   单测（`mini_asm.rs:1512`）喂的是裸 `["v0","x1"]`，绿灯不代表可达。
   riscv64/arm/mips 三张表 0 条访存：`lw sw sd ld sd ldr str lb sb lh sh movw movd` 全 0 命中。
2. **符号与重定位**：四张表的签名是 `(mnemonic: &str, operands: &[String]) -> Option<Vec<u8>>`
   （`mini_asm.rs:217/666/809`；mips 多一个 `is_64bit: bool`，`:999`）—— 返回值只能是最终字节，
   无 fixup 表达位，`bl foo`、`adr x0, sym` 不可能。后端自己有符号机制（`ensure_sym`、
   `relocatable`、`emit_bl_to`），缺的是接到 asm 文本通路。
3. **标签与分支**：表无 PC 相对分支臂；`known_mnemonics` 三个"分支名单"项（riscv `beq`、
   mips `beq`、arm `b`）的全部 operand 形状都解析不出；`LabelDef` → `NonCode`
   （`asm_source.rs:292-294`）不产字节。且交叉后端**没有** `Stmt::Label`/`Stmt::Goto` 发射臂
   （只有 x86_64 与共享校验有），aarch64 的 asm goto（`aarch64/emit/block.rs:199-202`）只做
   恒等 `%[name]` 文本替换，没有 x86_64 的 `c_label_to_asm` 前缀 → asm 标签与 C 标签同命名空间。
4. **文本方向（反汇编）**：四个交叉后端的 `-S` 函数体都是占位桩 —— 真发 `.globl`/`.type`/标签/
   内联 asm 原文，指令体只是固定桩（aarch64 `codegen/output.rs:93-95` = `// IR lowering` + `ret`；
   riscv64 `:70-71` 同形；arm `:53-58` = 注释 + `push {fp,lr}`/`mov fp,sp`/`mov r0,#0`/`pop {fp,pc}`；
   mips `baseline_mips.rs:1895-1897` = 注释 + `jr $ra`）。
   `GasTextOutput`（`shared/features.rs:51`）只出现在 `FeatureSet::full()`（`:143`），
   交叉后端走 `tier1_cross()`/`tier2_cross()`（`aarch64/backend.rs:22`、`riscv64/backend.rs:21`、
   `arm/backend.rs:19`）= **未声明**，不是"显式 false"。E0312（`codegen/mod.rs:1338`）仍是正确闸门。
5. **编码存疑**：`fadd`(s5)/`add`/`ld1w`/`st1w` 自标 `(simplified)`；`whilelt`（`:559`）同样按 rd
   低 3 位编码却**未标**。`ld1` 注释基址 `0x4C007400`（`:530`）与代码 `0x4c407400`（`:531`）不符，
   而 `st1` 用 `0x4c006400` —— 同族常数不同型，必有一处错。当前都被冻结基线按现状钉住。

### 错账（比缺能力更危险，先修）

6. `known_mnemonics(AArch64)`（`:364-366`）把 `"ld"`/`"st"` 列为已支持，表里没有这两条臂。
   grep 到的 `"ld"`/`"st"` 各 4 处是 dmb/dsb 的 domain 选项值（`0xd`/`0xe`）。
7. 本该拦住 6. 的护栏 `mnemonic_table_matches_mini_asm_dispatch`（`:812`）是假绿，且是
   "写了严判据又被自己废掉"：断言（`:835`）是 `body.contains(&arm) || 限本表那段.contains(&arm)`，
   而前一项对**整段** `body`（`split_once("pub fn encode_aarch64(")` 直到文件尾，含另三张表
   与测试模块）做子串匹配，于是 `"ld"` 靠 dmb 臂的选项值通过，第二个析取项永不参与判定；
   且它也不验可达性。同文件 `:845` 已有一条"某助记符不得默默产字节"的 `analyze()`
   行为断言，是现成的参照形状。
8. 覆盖面口径错位：`dcc-asm-unification-plan.md` §3.2 记 29 / 12 / 15 / 12；实测
   **臂数** 33 / 12 / 15 / 12，**助记符数** 29 / 12 / 16 / 13（or-pattern：`"svc" | "swi"`、
   `"nop" | "ssnop"`）。aarch64 的 29 无访存。

### 发射点与回填（方案 C 的前置）

9. 口径纠正：`self.emit(` 是 aarch64 **283** / riscv64 **190** 处（arm 83、mips 59 走
   `self.code.extend_from_slice`），但**不是**常量数组：aarch64 = 188 处 `&insn.to_le_bytes()`
   （调用点就地位运算拼 `u32`）+ 82 处字面量；riscv64 = 188 处变量 + 2 处字面量。
   该进规格的主体是位运算拼装，不是常量。
10. 占位—回填面比旧记录更大：交叉后端共 **24 个** `patch_*`/`emit_*_placeholder`
    函数定义（aarch64 10、riscv64 8、mips 4、arm 2），**9 条**手写 pending 队列
    （aarch64 `struct_def.rs:34-40` 4 条 / arm `:34-35` 2 / riscv64 `:54` 1 / mips
    `baseline_mips.rs:47-48` 2），元组形状 4 种：`(u64,String,BranchKind)` /
    `(u64,String,u8)` / `(u64,u8,u64)` / `(u64,String)`（`pending_bl_calls`）。
11. 一次产不出完整字节的形状（规格须能表达"1 条逻辑指令 → N 个 slot + fixup"）：
    ldr-literal 发两条、PC 基准 `at+8`（`patch.rs:30/41`）；`emit_adr_large` 预留 8 字节的
    ADRP+ADD 双占位（`:89`，走 `pending_adr_exec`）；riscv `patch_auipc_load`/`patch_auipc_ld`
    （`special/float_conv.rs:52/69`）；mips 延迟槽 `pending_instructions: Vec<MipsInstruction>`
    （`codegen/delay_slots/mod.rs:28`，回填不只是改字节，还要重排）。
12. fixup 类型**不需要新发明**：x86_64 已有 `PendingCodeRel { offset, symbol, rel_type, addend }`
    并在表驱动编码器里"emit 占位 + push reloc"（`asm/enc_alu.rs:498-505`、`ifunc.rs:1529-1535`、
    `asm/directive_emit.rs:35-52`、`elf/rel_symbols.rs:689`）。缺的是把它从 x86_64 私有提升为
    跨 ISA 共享（补 PC 基准与 slot 长度两个维度），不是设计新机制。
13. **已发生的失效（可达）**：`patch_adr`（`patch.rs:59-76`）在 ADR 越 ±1MiB 时应退化 ADRP+ADD，
    注释自陈"patch 是原地修改, 无法插入""超出时截断到最大可表示偏移"，实际只把原 slot 改成
    ADRP（丢 lo addend，rd 得页地址）就 `return`。触发链完整：唯一调用者是 `pipeline.rs:571`
    （drain `pending_adr_exec`），入队点 `ops.rs:39` 只占 **1 个 slot**；而"预留 2 slot"的正确修法
    `emit_adr_large`（`:89`）**零调用者 = 死码**；且接上也不够：越界分支只回填第一条，
    第二条停在 `add rd,rd,#0`（`:94`），`_lo_off`（`:63`）算了即丢。只在非 relocatable
    （ET_EXEC / 内核代码模型）下发生，relocatable 走 `R_AARCH64_ADR_PREL_LO21` 重定位
    （`ops.rs:23-33`）不受影响 —— 解释了为何长期未暴露。无测试覆盖。
14. PC 基准三种写法：`at`（`patch_bl:11`、`patch_adr:57`、riscv `patch_branch`/`patch_auipc_*`）、
    `at + 4`（`patch_b26:117`、`patch_cbz:127`、`patch_bcond:116`、`patch_cbnz:105`）、`at + 8`
    （`patch_ldr_u32:42`，那是 ldr-literal 双 slot 布局，另有理由）。前两组**在同一 `at` 定义下
    不可能都对**：`pos() = code.len()`（`ops.rs:307`），而 `b()`/`cbz_w0()`/`emit_bcond()`/
    `emit_bl_to()`/`atomic_insn` 五处都在 emit **之前**取 `at`（即分支自身地址），`label()`（`ops.rs:312`）
    与 `emit_label_raw()`（`ops2.rs:228`）登记的也是裸 `pos()`，无补偿。按 `patch_bl` 注释里的
    实测（"旧代码误用 at+4 使目标总是少 4 字节"），`at+4` 那四条应落到标签前一条指令。
    **但不写成定论**：其调用面是主通路（`self.b(` 23、`emit_bcond(` 15、`emit_cbnz` 26 处），
    若每条都差 4 则 aarch64 分支根本不可用 —— 必有一处我未看到，需一次 runtime 复核才能收口。

### A 要做的

- 护栏换成行为判据：对名单每项构造合法 GAS 文本走 `analyze()`，断言产出字节非空；
  不在名单者断言 `UnknownMnemonic`。
- 补负向用例：`ld1 {v0.16b}, [x0]`、`lw $t0, 4($t1)`、`ble $t0, .L`、`bl foo` 均 → 透传。
  第一条是全仓唯一**正向**断言可达性的用例（`:845` 那条只管"不得默默产字节"）。
- 删 `known_mnemonics(AArch64)` 的 `ld`/`st`（会改 E0311 文案，动手前先确认无快照钉住旧措辞）。
- 1.–3. 写成 P1/P2 的硬前置（`Operand` 能表达 Mem 及三种括号语法；返回值带 `Vec<Fixup>`；
  有 Label 引用与松弛修补；名单与表同源生成），**不得**在待删的 match 链上加解析。
- 8. 的覆盖面名单改成"臂数 / 助记符数"两列对照。
- 新建 `docs/aarch64_encoding_defects.md`，登记 5. 的全部条目 + 13. + 14.（后者是实测项）。

## B. P12 落地后的过期口径：6 处

P12（物理寄存器家 + 溢出回填）已落地，实现见 `src/ssa/regalloc.rs`、
`docs/p12-physreg-spill-design.md`。下列文字仍停在旧世界，只改注释与 Markdown。

| # | 位置 | 过期说法 | 现状 | 修法 |
|---|---|---|---|---|
| F1 | `src/ssa/regalloc.rs:39-41` | 准入条件 6：`-g` ⇒ 整条 pass 不动，DWARF 只会发栈槽 | 同文件 `register_homes_allowed()`（L465）恒 `true`，测试 `debug_info_no_longer_gates_the_register_home`（L1346）钉住 | 条件 6 改为「`-g` 不再拦，Phase 6.2 起发 `DW_OP_regN`」 |
| F2 | `src/ssa/regalloc.rs:117` | 「默认只等于 `POOL`，延伸池已被实测否决」 | L128-131 env 未设即返回 `AUTO_EXT_POOL`（默认开），且同注释 L123-124 说的是反话 | 删 L117 一句，保留逐号否决理由 |
| F3 | `src/ssa/passes.rs:974-979`、`src/ssa/regalloc.rs:13-15` | 「需单独立项」「不属本方案范围」 | 立项已完成并合入 | 两句标注「2026-10 已完成」；`passes.rs` 同块第 1、2 条删除理由仍有效，不动 |
| F4 | `docs/support_features.md:31 / 534 / 573` | 在售特性写的是已删 stub：`asm_reg_pressure`、`__dcc_regalloc_done` hint、「分配 14 个通用寄存器」、「溢出终点最远的区间」、P12 记为「SSA 值计数」 | `src/` 只剩两处历史提及（`rebuild.rs:447`、`passes.rs:967`），无任何实现；真实候选表 `POOL` 4 个（`regalloc.rs:66`）+ `AUTO_EXT_POOL` 2 = 6 个号，溢出走 `Location::Spill` | 三行按现状重写，指向设计文档 |
| F5 | `docs/ssa-pass-upgrade-plan.md:59 / 68` | P12 标「⚠️ stub/hint」「没有真正 SSA 层面分配」 | 与同文件 L24「✅ 全部落地（2026-10-09 终版）」冲突；设计文档 §11.61（`p12-physreg-spill-design.md:3419-3423`）只回了主状态表（P16 与 P12 行），**漏了 L59/L68 两处旧列表** | L59/L68 按 L24 回写；§11.61 末尾追加一行订正，不重写整段 |
| F6 | `docs/ssa-pass-upgrade-plan.md:331-339` | 三条「仍挡着」的 blocker + 结论「P12 仍不能宣称完成」 | 「结果恒在 `%rax`（194 处）」实测 `gen_expr(.., 0)` **0** 处 / `EVAL_BASE` 305 处 / `eval_holding` 11 处，已改走二者；「序言只有 4 条 callee-saved」已不阻挡（caller-saved 走 persist/restore 窗口）；只剩「声明级家 vs 程序点级存活」，且属设计边界 | 更新三条读数，结论改为「已闭 1.5 条，剩一条是边界不是欠账」 |

### B 不要动

- `ssa-pass-upgrade-plan.md:341-345` 原文 —— L339 已声明是历史保留。
- `passes.rs:964-972` 前两条删除理由 —— 仍有效。
- `regalloc.rs:450`「不做全局干涉图」—— 下一句已自我订正。
- `rebuild.rs:447` 的 `__dcc_regalloc_done` 举例 —— 名单形状没变，可加一句「该 pass 已删」。

### B 验收

1. `grep -rn "不属本方案范围\|只会发栈槽\|需单独立项" src/ssa/` 每条命中都带现状标注。
2. `grep -rn "asm_reg_pressure\|__dcc_regalloc_done" docs/support_features.md` 零命中。
3. `check_reproducible_output.sh` 与 `check_opt_equivalence.sh` 读数不变，产物 diff 行 0。

## 复取命令

```bash
grep -rn 'self\.emit(' src/codegen/aarch64 | wc -l                                        # 283（riscv64 190）
grep -rnE 'fn \w*(patch|placeholder)\w*\(' src/codegen/{aarch64,riscv64,arm,mips} | wc -l   # 24 = 10/8/4/2
grep -rnE 'extend_from_slice\(&[A-Za-z0-9_]+\.to_(le|be)_bytes' src/codegen/{arm,mips} | wc -l    # 83 + 59
grep -rnE "strip_prefix\('\[|starts_with\('\[" src/codegen | wc -l                        # 0
```

> 行号与计数均用工具链无关的口径（命中的是行，不是字符）；本仓部分源文件含混合行尾，
> 用 `Get-Content` 数组下标切行会与 `grep`/编辑器的行号不一致，不宜拿来做行号引用。

## 未实测

本机 `cargo build --bin dcc` 失败于 `dlltool.exe` 缺失（依赖层，与本问题无关），
故未跑任何测试、未做交叉 objdump 的位型验证。上述结论均来自源码与被冻结的基线文件。
14. 的 PC 基准冲突需一次端到端 `-c` + objdump 才能定论。
