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
| `-S` 回归 | `integration.rs` + `scripts/remote_feature_check.sh`（14 项特性探测） |
| 模块扩展回归 | SysV sret、va_list XMM、asm `%[name]`、`-MMD`/`-MP`、LTO size、`-mno-red-zone` |
| GNU case range / 逗号数组声明符 | `bounds.c` 头文件依赖、`integration.rs` 回归 |

## 阻塞级（Linux 6.6 全树）

| 特性 | 当前首个失败点 | 状态 |
|------|----------------|------|
| prepare0 全链路 | `make` 触发 `kernel/bounds.s` 等 prepare 依赖 | ⚠️ 单 TU 已通，全 make 待验 |
| QEMU 启动矩阵 | 远程无 sudo/QEMU，未生成 bzImage | ❌ 脚本就绪 `scripts/qemu_boot_matrix.sh` |
| pahole / objtool 完整回归 | 需完整 vmlinux 链接 + 工具链 | ❌ |
| modpost `file2alias.c` | 单 TU `-c` 生成 `file2alias.o`（2026-07-15） | ✅ |
| LTO 链接器消费 | `.llvm.lto` 可自 ELF 提取/合并；ld 侧 dead symbol 探测已就绪 | ⚠️ |

## 远程 debug 记录（2026-07-15）

| 问题 | 根因 | 修复 |
|------|------|------|
| `remote_feature_check.sh` 报 `pipefail: invalid option` | Windows 同步后脚本为 CRLF | `.gitattributes` 强制 `*.sh` LF；提交前 `git ls-files --eol` 确认 `w/lf` |
| `devicetable-offsets.c` 缺 `generated/autoconf.h` | 内核树未跑 `syncconfig` | `sync_remote_build.sh` 自动 `make include/generated/autoconf.h` |
| `file2alias.c` 编译失败 | 1) `seen` 阻止 stddef 重入 2) `<stddef.h>` 误解析为 linux/stddef.h | 移除 include-once；GCC include 优先于 linux UAPI |

## 已实现模块（2026-07-15 扩展）

| 特性 | 说明 |
|------|------|
| SysV AMD64 调用 | `emit_sysv_call`：寄存器/栈/XMM、Memory 类 sret、AL=vector 计数 |
| va_list | 24B struct；GP+XMM reg_save_area（176B）；`va_start`/`va_arg`/`va_copy` |
| 内联 asm | AT&T 编码；`%[name]` 命名操作数；Ident LEA；GlobalAsm |
| Host stdio/modpost 前端 | 函数 typedef、`restrict`、GCC include 路径、逗号指针声明符、`__asm__` 字符串拼接 |
| LTO 链接探测 | `elf::read_section_data` + `lto::extract_from_object` / `collect_from_objects` / `dead_local_functions` |
| `-MD`/`-MMD`/`-MP` | include 图 → `.d`；`-MMD` 过滤系统头；`-MP` phony 规则 |
| 优化 | `-O1+` 常量折叠；`-O2+` 纯表达式语句 DCE |
| 机器选项 | `-mno-red-zone` 128B 保留；`-fno-omit-frame-pointer`；`-mno-sse` |
| 调试信息 | DWARF4 真实 `low_pc`；BTF 头；eh_frame/ORC 占位 |
| Sanitizer | TU 级一次 init 桩 + `.dcc.san_info` |
| LTO | codegen 后 `.llvm.lto` 含真实 `func_sizes` |
| Host include | GCC 内建 include 路径探测 |

## 远程基线

- 工作区：`~/dcc-agent-linux66`（不覆盖旧 `~/dcc`）
- 同步构建：`scripts/sync_remote_build.sh`（rsync + `cargo test` + feature check + devicetable/bounds）
- 快速验收：`scripts/remote_feature_check.sh`（14 项：O1/MD/MMD/MP/san/LTO/asm/`-S`/va_list 等）
- 内核树：`~/linux-6.6`，需已有 `.config`；首次缺 `autoconf.h` 时脚本会 `make olddefconfig` + 生成头文件
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
