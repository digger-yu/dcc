# va_arg 跨 TU（2 文件）gcc 差分用例

这里的用例必须是 **callee.c + caller.c 两个翻译单元分别用 gcc / dcc 编译后混合
链接**才能判定，所以不能放进 `../va_arg/`（那里由 `dcc_gcc_diff.sh *.c` 单文件
全量扫，也进不了 `include_str!` 型 cargo 夹具）。

跑法：

```sh
scripts/dcc_gcc_diff.sh --mix <name>_callee.c <name>_caller.c
```

四组前缀均已注册为常驻用例（`tests/integration/c_abi_vararg.rs` 里的
`va_arg_mix_*_cross_tu_gcc_interop`，走 `run_gcc_two_tu_cross_check`，它做的
就是上面那条 `--mix` 的四方向对照），所以 CI 不依赖手工命令。

`--mix` 同时比退出码**与 stdout**。这些 callee 把期望值与实际值打成文本后让
`main` 恒返回 0，只比退出码会把错判成 MATCH。

| 前缀 | 覆盖 | 判据 |
|---|---|---|
| `m128_shared_slot_*` | `va_arg(ap, __m128d)` 的两个 eightbyte 共享一个 16 字节 XMM 槽；曾出现"低半进寄存器 + 高半进栈"的拆分 | dcc callee + gcc caller 与全 gcc 参照逐字节一致 |
| `int128_11named_*` | 11 个命名参数占满 6 个 GP 寄存器（`gp_offset == 48`），`va_arg(ap, __int128)` 全在 `overflow_arg_area`；曾出现低半对、高半恒为同一个栈残值 | 同上 |
| `ld80_vararg_*` | 变参 `long double` 属 X87 类，永远走 16 字节 `overflow_arg_area` 且内容是**真 80-bit 编码**；曾发 `movsd` 只读低 8 字节（即读到 64 位尾数当成 f64，`(long long)` 得 LLONG_MIN） | 同上；**四个方向**（全 gcc / 全 dcc / dcc callee+gcc caller / gcc callee+dcc caller）全部验证 |
| `ld80_namedparam_*` | 具名（非变参）`long double` 形参槽内必须是 80-bit 编码、callee 入口需 `fldt`→`fstpl` 降级，且 X87 **标量返回值在 st(0)**（旧代码把 `abi_classes==[X87]` 误判成 sret → gcc caller 不填 rdi → SIGSEGV） | 同上；caller 同时校验形参回写与返回值的整数/小数部分，四项判据缺一不可 |

两例的根因与单文件版 `../va_arg/int128_loop_local_full_width.c`、
`../va_arg/sseup_shares_one_slot.c` 相同（`__int128` 局部量只落 8 字节 /
FP 槽按 XMM 寄存器分组），留在这里是因为它们额外约束了**跨编译器**的 ABI 边界：
caller 由 gcc 生成，所以任何偏离都只能来自 dcc 的读侧。
