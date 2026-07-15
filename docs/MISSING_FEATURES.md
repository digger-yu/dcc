# DCC Linux 6.6 缺失特性（按自动化证据）

> 最后更新：2026-07-15（远程 debug）— 以 `scripts/test_kernel_features.sh`、
> `scripts/remote_feature_check.sh`、`scripts/sync_remote_build.sh` 与
> 远程 Linux 6.6 Kbuild 日志为准。

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
| `-S` 汇编输出 | 完整 C 函数机器码反汇编；lock/SSE 前缀指令；自定义节按 `.section` 分组（与 `.o` 布局一致） |
| `-S` 回归 | `integration.rs` + `scripts/remote_feature_check.sh`（custom section / lock cmpxchg） |
| GNU case range / 逗号数组声明符 | `bounds.c` 头文件依赖、`integration.rs` 回归 |

## 阻塞级（Linux 6.6 全树）

| 特性 | 当前首个失败点 | 状态 |
|------|----------------|------|
| prepare0 全链路 | `make` 触发 `kernel/bounds.s` 等 prepare 依赖 | ⚠️ 单 TU 已通，全 make 待验 |
| QEMU 启动矩阵 | 远程无 sudo/QEMU，未生成 bzImage | ❌ 脚本就绪 `scripts/qemu_boot_matrix.sh` |
| pahole / objtool 完整回归 | 需完整 vmlinux 链接 + 工具链 | ❌ |
| modpost `file2alias.c` | host 工具源码体量大，parser 仍有缺口 | ❌ 单 TU 编译失败 |
| LTO 链接器消费 | `.llvm.lto` 已生成，ld 侧未接入 | ⚠️ |

## 远程 debug 记录（2026-07-15）

| 问题 | 根因 | 修复 |
|------|------|------|
| `remote_feature_check.sh` 报 `pipefail: invalid option` | Windows 同步后脚本为 CRLF | `.gitattributes` 强制 `*.sh` LF；提交前 `git ls-files --eol` 确认 `w/lf` |
| `devicetable-offsets.c` 缺 `generated/autoconf.h` | 内核树未跑 `syncconfig` | `sync_remote_build.sh` 自动 `make include/generated/autoconf.h` |
| `file2alias.c` 编译失败 | modpost 源码 parser 缺口 | 跟踪中，勿全局设 `HOSTCC=dcc` |

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
- 同步构建：`scripts/sync_remote_build.sh`（rsync + `cargo test` + feature check + devicetable/bounds）
- 快速验收：`scripts/remote_feature_check.sh`（O1/MD/sanitizer/LTO/GlobalAsm/`-S`）
- 内核树：`~/linux-6.6`，需已有 `.config`；首次缺 `autoconf.h` 时脚本会 `make olddefconfig` + 生成头文件
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
