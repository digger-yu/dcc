# AArch64 编码缺陷登记册（G11 encoding-correctness defect ratchet）

> 本文档是 G11 的「缺陷棘轮（defect ratchet）」。
> 迁移（pure-move）期间，凡「意图 ≠ 实际发射字节」的编码缺陷，**只登记、不修复**。
> 基线一旦冻结，本清单**只允许变短**：修复后条目移入「已修复（Resolved）」段。
> 不得新增未经验证的缺陷；不得用「看起来像」的猜测条目污染基线。

行号以当前工作树为准（`docs/dcc-asm-unification-plan.md` §2 曾给出的行号在迁移过程中已漂移）。
所有字节均以 `aarch64-linux-gnu-as` + `aarch64-linux-gnu-objdump` 验证：将字节以 `.inst 0xXXXXXXXX` 汇编后反汇编，
对照其被识别成的助记符。

## 缺陷表

| ID  | 发射点 (file:line)                              | 意图                              | 实际发射 u32          | objdump/as 真相                          | 状态   |
|-----|-------------------------------------------------|-----------------------------------|-----------------------|------------------------------------------|--------|
| F1  | `src/codegen/aarch64/emit/expr/fcmp.rs:12`      | `fcmp s0, s1`                     | `0x1e280000`          | `fcvtps w0, s0`（浮点比较变成无符号取整）| open   |
| F2  | `src/codegen/aarch64/expression_impl.rs:108`    | `mov x12, x0`（存基址，64-bit）   | `0x2a000?0c` 即 sf=0  | `orr w12, w0, wzr`（截断成 32 位）       | open   |
| F3  | `src/codegen/aarch64/expression_impl.rs:363`    | `mov x12, x0`（存基址，64-bit）   | sf=0（同上 helper）   | `orr w12, w0, wzr`（截断成 32 位）      | open   |
| F4  | `src/codegen/aarch64/expression_impl.rs:397`    | `mov x12, x0`（存基址，64-bit）   | sf=0（同上 helper）   | `orr w12, w0, wzr`（截断成 32 位）      | open   |
| F5  | `src/codegen/aarch64/expression_impl.rs:423`    | `mov x12, x0`（数组基址）         | sf=0（同上 helper）   | `orr w12, w0, wzr`（截断成 32 位）      | open   |
| F6  | `src/codegen/aarch64/expression_impl.rs:428`   | `mov x9, x0`（下标，64-bit）      | sf=0（同上 helper）   | `orr w9, w0, wzr`（截断成 32 位）       | open   |
| F7  | `src/codegen/aarch64/expression_impl.rs:449`    | `mov x9, x0`（下标，无缩放下）     | sf=0（同上 helper）   | `orr w9, w0, wzr`（截断成 32 位）      | open   |
| F8  | `src/codegen/aarch64/expression_impl.rs:535`    | `mov x12, x0`                      | sf=0（同上 helper）   | `orr w12, w0, wzr`（截断成 32 位）      | open   |
| F9  | `src/codegen/aarch64/expression_impl.rs:550`    | `mov x12, x0`（数组基址）         | sf=0（同上 helper）   | `orr w12, w0, wzr`（截断成 32 位）      | open   |
| F10 | `src/codegen/aarch64/expression_impl.rs:553`    | `mov x9, x0`（下标，64-bit）      | sf=0（同上 helper）   | `orr w9, w0, wzr`（截断成 32 位）       | open   |
| F11 | `src/codegen/aarch64/special/builtins.rs:102`   | `mov x10, x0`                      | sf=0（同上 helper）   | `orr w10, w0, wzr`（截断成 32 位）      | open   |
| F12 | `src/codegen/aarch64/special/builtins.rs:123`   | `mov x10, x0`                      | sf=0（同上 helper）   | `orr w10, w0, wzr`（截断成 32 位）      | open   |
| F13 | `src/codegen/aarch64/emit/expr/bitfield.rs:27`  | `mov x12, x0`                      | sf=0（同上 helper）   | `orr w12, w0, wzr`（截断成 32 位）      | open   |
| F14 | `src/codegen/aarch64/expression_impl.rs:432`    | `lsl x9, x9, #1`                   | `0xd3000109`          | `.inst undefined`                       | open   |
| F15 | `src/codegen/aarch64/expression_impl.rs:435`    | `lsl x9, x9, #2`                   | `0xd3000209`          | `.inst undefined`                       | open   |
| F16 | `src/codegen/aarch64/expression_impl.rs:438`    | `lsl x9, x9, #3`                   | `0xd3000309`          | `.inst undefined`                       | open   |
| F17 | `src/codegen/aarch64/expression_impl.rs:441`    | `lsl x9, x9, #4`                   | `0xd3000409`          | `.inst undefined`                       | open   |
| F18 | `src/codegen/aarch64/emit/expr/float.rs:34`     | `orr v1.16b, v0.16b, v0.16b`       | `0x4e810020`          | `.inst undefined`                       | open   |
| F19 | `src/codegen/aarch64/emit/expr/float.rs:38`     | `movi v0.4s, #0`                   | `0x4e800000`          | `.inst undefined`                       | open   |
| F20 | `src/codegen/aarch64/emit/expr/float.rs:42`     | `movi v1.4s, #0`                   | `0x4e800001`          | `.inst undefined`                       | open   |
| F21 | `src/codegen/aarch64/emit/expr/float.rs:26`     | `mov s1, s0`                        | `0x1e200020`          | `fcvtns w0, s1`（浮点移动 → 取整）      | open   |
| F22 | `src/codegen/aarch64/emit/expr/float.rs:30`     | `fneg s0, s0`                       | `0x1ea00000`          | `.inst undefined`                       | open   |

> 说明：
>
> - F2..F13 全部经由同一个 helper `emit_mov_x_reg`（`ops2.rs:105-109`）发射，
>   helper 当前用的基址是 `0x2a000000`（`ORR Wd, WZR, Wn`，sf=0），
>   而 64 位版本应为 `0xaa000000`。这是同一缺陷的多个调用点。
> - 计划 §2 说「11 个 mov xN,x0 发射点」，当前工作树中 helper 调用点共 **12** 处（见 F2..F13），
>   差异是迁移拆分后新增/暴露的调用点。两者都在同一根因（sf=0）下。
> - 计划 §2 说「HFA / 向量 / `fsub` / `fdiv` 共 10 个字 `.inst undefined`」。
>   当前工作树能在源码里直接找到的 `undefined`/错误字只有 F14..F22（共 9 行 / 11 个字）。
>   其中 `emit_fsub`（`float.rs:90`, `0x1e203800`）与 `emit_fadd`（`float.rs:80`, `0x1e202800`）
>   经 objdump 验证**是合法的 `fsub` / `fadd` 助记符**，不算缺陷；
>   未发现 `emit_fdiv` 发射点。剩余 `stp s` / HFA 成对 store 在 `load_hfa_to_vregs` /
>   `str_st_sp` 里使用的 `0xbd000000`、`0xfd000000` 也是合法的 `str s` / `str d` 助记符。
>   因此 F23..F32 暂缺——待 G11 阶段再用更系统的测试补齐（不编造条目）。

## 如何复现 / 验证

```bash
# 1) 把待验证的字写进一个 .inst 列表
cat > /tmp/probe.S <<'EOF'
    .text
fn:
    .inst 0x1e280000   # F1
    .inst 0x2a0003e0   # F2..F13 的基址（ORR Wd, WZR, Wn）
    .inst 0xaa0003e0   # 对照：正确的 64 位 MOV
    .inst 0xd3000109   # F14
    .inst 0x4e810020   # F18
    .inst 0x4e800000   # F19
    .inst 0x4e800001   # F20
    .inst 0x1e200020   # F21
    .inst 0x1ea00000   # F22
EOF

# 2) 汇编 + 反汇编
aarch64-linux-gnu-as /tmp/probe.S -o /tmp/probe.o
aarch64-linux-gnu-objdump -d /tmp/probe.o
```

若 objdump 读到 `.inst 0xXXXXXXXX ; undefined` 或助记符与代码注释不符，即确认为缺陷。

## 已修复（Resolved）

（空。修复一条、从 open 段下移一条。）
