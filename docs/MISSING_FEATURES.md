# DCC Linux 6.6 缺失特性（按自动化证据）

> 最后更新：2026-07-15 — 以 `scripts/test_kernel_features.sh` 与远程 Linux 6.6 Kbuild 日志为准。

## 已通过的门禁

| 门禁 | 证据 |
|------|------|
| typed IR + 严格语义 | `cargo test` integration（static_assert / unknown typedef / int128） |
| SysV va_list（GP） | `examples/test_p4_va_list.c` |
| asm goto + computed goto | `examples/test_asm_goto.c` |
| 自定义 ELF section / WEAK / HIDDEN | `examples/test_p2_elf_attrs.c` + readelf 断言 |
| __int128 / register asm / 全局符号 | `examples/test_p1_features.c` |
| 预处理 forced-include 注释安全 | `tests/fixtures/preproc_comment_force.h` |

## 阻塞级（Linux 6.6 全树）

| 特性 | 当前首个失败点 | 状态 |
|------|----------------|------|
| mod/devicetable 生成 TU | `scripts/mod/devicetable-offsets.c` 宏展开后类型/标识符解析 | ❌ |
| 完整 DWARF + BTF | 未生成 `.debug_*` / pahole 不可用 | ❌ |
| objtool / ORC 帧信息 | 帧链不完整；远程无完整 objtool 回归 | ❌ |
| KASAN/UBSAN/KCSAN/KCOV 插桩 | IR 插桩未实现 | ❌ |
| 跨 TU LTO | `src/lto.rs` 摘要容器占位，链接器未消费 | ❌ |
| `.S` / `-x assembler-with-cpp` | 未接入独立汇编驱动 | ❌ |
| QEMU 启动矩阵 | 远程无 sudo/QEMU，未生成 bzImage | ❌ |

## 已实现但需扩展

| 特性 | 说明 |
|------|------|
| SysV AMD64 调用 | `emit_sysv_call`：寄存器/栈/sret、AL=vector 计数 |
| va_list | 24 字节 struct + `va_start`/`va_arg`/`va_copy` |
| 内联 asm | 约束解析 + AT&T 编码器；文件级 `asm()` → `Decl::GlobalAsm` |
| `-MD` 依赖 | 预处理记录 include 图并写入 `.d` |
| 优化 | `-O1+` 常量折叠（`src/opt.rs`） |
| 机器选项 | `-mno-red-zone`、`-fno-omit-frame-pointer`、`-mno-sse` 接入 codegen |

## 远程基线

- 工作区：`~/dcc-agent-linux66`（不覆盖旧 `~/dcc`）
- 内核树：`~/linux-6.6`，`make CC=~/dcc-agent-linux66/target/release/dcc` 推进至 `devicetable-offsets.c`
- 磁盘：单内核树 + `make mrproper`  между配置
