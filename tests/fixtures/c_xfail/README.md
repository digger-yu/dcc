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
`tests/integration/c_asm.rs::flag_clobber_frame`，断言 stdout 含 `FAIL:`
（自证 `ok = 0` 执行过）且 **rc == 1**。夹具中的 `!= 99.0` 仍是**故意的
触发器**，不要"修回" `10.0`，否则 FAIL 分支不执行、测试失去意义。

## `ld_global_read.c` —— ✅ 已修复，已迁移到 `c/`

原缺陷：`long double` **全局量**的读侧仍发 `movsd`（只取 80-bit 对象的低 8 字节当 f64），
读回 `-nan`；发射侧当时已修（`.data` 与 gcc 逐字节一致）。判据 `r` 当时 =14（缺 bit0）、rc=1。

- **现状（2026-09-28 实测）**：读侧已改回 x87 `fldt`，`dcc -std=gnu23` 编译链接运行得
  `r=15 want=15`、`gh=1.000000000000000000000000000`、无 `-nan`、**rc=0** —— 四项判据全真。
- **处置**：按本 README 已有的 `flag_clobber_frame.c` 先例，夹具 `git mv` 回 `tests/fixtures/c/ld_global_read.c`，
  用例从「断言失败」改为「钉住修好后的具体值」的不得回退门（同时管住两侧：`.data` 里必须是真 80-bit
  位型；`<main>` 与链接产物里读 `<gh>` 必须是 `fldt` 而不是 `movsd`），落点
  `tests/integration/c_lang.rs::ld_global_read_long_double_uses_80bit_load_and_exact_data`（主体：x87 80-bit
  数据布局与装载，属通用 C 语义簇）。
- **历史根因与两侧反汇编对照**见 `docs/longdouble_80bit_migration.md`。

本目录当前已无 xfail 用例（§7.1 的 `c_xfail/` = 已知预期失败）；保留目录与本文档作为
后续跨领域已定位未修缺陷的存放约定。
