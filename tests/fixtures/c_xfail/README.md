# 已知失败用例（跨领域）

`va_arg_xfail/` 只放变参相关，这里放**不属于任何既有子领域**的已定位未修缺陷。

## `flag_clobber_frame.c` —— ✅ 已修复，已迁移

原缺陷：局部 `int ok` 的赋值 `ok = 0;` 确实执行，但函数末尾 `if (!ok)
return 1;` 没有触发（rc=0 假绿）。

- **根因**：`pre_register_vars`（`src/codegen/x86_64/emit/func_setup.rs`）
  按名字去重共享栈槽，但槽容量按**首次声明**定容。夹具里 4 个 block 都叫
  `s`：先 16B 的 `struct with_vec s`（槽 `-0x50(%r13)`，2 槽），后 32B 的
  `struct vec_int s` 沿用同一槽，其字段 `s.i`（偏移 +16 → `-0x40(%r13)`）
  正是相邻局部 `ok` 的槽——`s.i = 7` 把刚写下的 `ok = 0` 覆盖回 7。
- **修复**：预注册先全树收集每个名字的最大分配需求（含嵌套 block 与
  switch case），按最大值定容；分配口径收敛到单一真源
  `Codegen::var_alloc_slots`，`register_var` 同步复用。
- **此前的"触发条件是 frame 布局依赖、需 T1+T2 一起"二分结论是假象**：
  那轮二分用的是 `!= 10.0`（已能 PASS）的 T3，FAIL 分支根本没执行，
  所以看似不触发。把 T3 判据改成 `!= 99.0` 后，仅 T3+T8 即可稳定复现。

本文件现位于 `../c/flag_clobber_frame.c`（绿灯夹具），并注册为
`tests/integration.rs::flag_clobber_frame`，断言 stdout 含 `FAIL:`
（自证 `ok = 0` 执行过）且 **rc == 1**。夹具中的 `!= 99.0` 仍是**故意的
触发器**，不要"修回" `10.0`，否则 FAIL 分支不执行、测试失去意义。

本目录当前已无 xfail 用例；保留目录与本文档作为后续跨领域已定位未修
缺陷的存放约定。

## `ld_global_read.c`（未修）

`long double` 全局量读侧仍发 `movsd`（只取 80-bit 对象的低 8 字节当 f64），读回 `-nan`。
发射侧已修：`.data` 现在与 gcc 逐字节一致。判据 r 当前 =14（bit0 缺），修好后应为 15、rc=0。
根因、两侧反汇编与下一步见 `docs/longdouble_80bit_migration.md`。
