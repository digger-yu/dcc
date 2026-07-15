# DCC Linux 6.6 缺失特性（按自动化证据）

> 最后更新：2026-07-15 — 以 `scripts/test_kernel_features.sh`、
> `scripts/sync_remote_build.sh` 与远程 Linux 6.6 Kbuild 日志为准。

## 已通过的门禁

| 门禁 | 证据 |
|------|------|
| typed IR + 严格语义 | `cargo test` integration（static_assert / unknown typedef / int128） |
| SysV va_list（GP） | `examples/test_p4_va_list.c` |
| asm goto + computed goto | `examples/test_asm_goto.c` |
| 自定义 ELF section / WEAK / HIDDEN | `examples/test_p2_elf_attrs.c` + readelf 断言 |
| __int128 / register asm / 全局符号 | `examples/test_p1_features.c` |
| 预处理 forced-include 注释安全 | `tests/fixtures/preproc_comment_force.h` |
| mod/devicetable 生成 TU | `devicetable-offsets.c` 直接 `-S` 输出 50+ `SIZE_*` 行 |
| DWARF + BTF（初版） | `-g` 生成 `.debug_*` + `.BTF` 节（integration 断言） |
| objtool / ORC 帧信息（初版） | `-g`/`-fno-omit-frame-pointer` 生成 `.eh_frame`、`.orc_unwind_ip`、`.cfi_*` |
| KASAN/UBSAN/KCSAN/KCOV 插桩（初版） | `-fsanitize=*` 函数入口桩 + `.dcc.san_info` 节 |
| 跨 TU LTO（初版） | `-flto` 生成 `.llvm.lto` 摘要节 |
| `.S` / `-x assembler-with-cpp` | `driver.rs` 预处理 + 调用系统 `as` |
| GNU case range / 逗号数组声明符 | `bounds.c` 头文件依赖、`integration.rs` 回归 |

## 阻塞级（Linux 6.6 全树）

| 特性 | 当前首个失败点 | 状态 |
|------|----------------|------|
| prepare0 全链路 | `make` 触发 `kernel/bounds.s` 等 prepare 依赖 | ⚠️ 单 TU 已通，全 make 待验 |
| QEMU 启动矩阵 | 远程无 sudo/QEMU，未生成 bzImage | ❌ 脚本就绪 `scripts/qemu_boot_matrix.sh` |
| pahole / objtool 完整回归 | 需完整 vmlinux 链接 + 工具链 | ❌ |
| LTO 链接器消费 | `.llvm.lto` 已生成，ld 侧未接入 | ⚠️ |

## 已实现但需扩展

以下模块已有初版实现，本轮扩展后远程 `cargo test --release` 与
`scripts/test_kernel_features.sh` 均通过：

| 特性 | 说明 |
|------|------|
| SysV AMD64 调用 | `emit_sysv_call`：寄存器/栈/sret、AL=vector 计数 |
| va_list | 24 字节 struct + GP/FP `va_start`/`va_arg`/`va_copy` |
| 内联 asm | 约束解析 + AT&T 编码；文件级 `asm()` → `Decl::GlobalAsm`（-c 导出 `.globl`） |
| `-MD` 依赖 | 预处理记录 include 图并写入 `.d` |
| 优化 | `-O1+` AST 常量折叠（`src/opt.rs`） |
| 机器选项 | `-mno-red-zone` 帧保留 128B、`-fno-omit-frame-pointer`、`-mno-sse` |
| 调试信息 | `src/debug.rs`：DWARF4 最小 CU + BTF 头 + eh_frame + ORC |
| Sanitizer | `src/sanitizer.rs`：AST 入口桩 + `.dcc.san_info` 节 |
| LTO | `src/lto.rs`：TU 摘要序列化/合并 + 单元测试 |
| Host include | GCC 内建 include 路径探测（`stdbool.h` 等 modpost 依赖） |

## 远程基线

- 工作区：`~/dcc-agent-linux66`（不覆盖旧 `~/dcc`）
- 同步构建：`scripts/sync_remote_build.sh`（rsync + `cargo test` + devicetable/bounds）
- 内核树：`~/linux-6.6`，`make CC=~/dcc-agent-linux66/target/release/dcc`
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
