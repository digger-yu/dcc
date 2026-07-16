# DCC Linux 6.6 缺失特性（按自动化证据）

> 最后更新：2026-07-16 — 以 `scripts/sync_remote_build.sh`、
> `scripts/prepare0_check.sh`、`scripts/pahole_regress.sh`、
> `scripts/objtool_regress.sh`、`scripts/remote_libiberty_probe.sh`
> 与远程 Linux 6.6 / GCC libiberty 日志为准。

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
| DWARF + BTF（含 struct） | `-g` 生成 `.debug_*` + `.BTF`（含 `dcc_probe` STRUCT 类型） |
| objtool / ORC 帧信息 | `-g`/`-fno-omit-frame-pointer` 生成 CIE+FDE `.eh_frame`、`.orc_unwind_ip` |
| KASAN/UBSAN/KCSAN/KCOV 插桩（初版） | `-fsanitize=*` 函数入口桩 + `.dcc.san_info` 节 |
| 跨 TU LTO | `-flto` 生成 `.llvm.lto`；`-r` 多 `.o` 经 `ld -r` + objcopy 合并摘要 |
| `-S` 汇编输出 | lock/SSE 前缀；自定义节按 `.section` 分组 |
| modpost `file2alias.c` | 单 TU `-c` 生成 `file2alias.o` |
| prepare0 窄目标 | `scripts/prepare0_check.sh`（devicetable / bounds / file2alias / empty.o） |
| pahole / objtool 回归脚本 | `scripts/pahole_regress.sh`、`scripts/objtool_regress.sh` |
| `#if` 十六进制字面量 | `preproc` 单测 `if_hex_*`（原 `0x41` 被拆坏） |
| `#include_next` | `preproc` 单测 + glibc `<limits.h>` |
| 宏展开涂蓝 / 防栈溢出 | `nested_function_macro_*`；远程 `argv.c` 不再 abort |
| `_Generic` + 三元控制表达式 | `tests/fixtures/generic_ternary.c`；远程 `libiberty/argv.c` |

## 阻塞级（Linux 6.6 全树）

| 特性 | 当前状态 | 说明 |
|------|----------|------|
| prepare0 全链路 `make prepare0` | ⚠️ 窄目标已通过 | 完整 make 仍可能触发无关 prepare 步骤；用窄脚本验收 |
| QEMU 启动矩阵 | ⚠️ 脚本就绪 | `scripts/qemu_boot_matrix.sh` 在无 bzImage 时 SKIP；需 `make CC=dcc bzImage` |
| pahole / objtool 完整回归 | ⚠️ 单 TU 已通过 | 完整 vmlinux 链接后 objtool 全树扫描仍待验 |
| LTO 链接器 dead symbol | ⚠️ 摘要已合并 | `dead_local_functions` 探测就绪；未接入 `--gc-sections` 删除 |

## 短期：GCC libiberty（纯 C）

详见 `docs/GCC_BUILD_GUIDE.md`。远程基线（2026-07-16）：**90 OK / 39 FAIL**。

| 缺口 | 影响 |
|------|------|
| 无 C++ | 无法编 GCC 主体 / 完整 bootstrap |
| 浮点 `AndAnd`/`OrOr` | `copysign.c` 等 |
| texinfo `@deftypefn` 注释 | 部分 libiberty 文件把文档当代码 |
| `_Generic` 指针精确匹配 | 当前指针一律兼容，可能选错 association |
| 复杂 demangle / regex / obstack | 仍有解析或类型错误 |

## 远程 debug 记录（2026-07-15）

| 问题 | 根因 | 修复 |
|------|------|------|
| `remote_feature_check.sh` 报 `pipefail: invalid option` | Windows 同步后脚本为 CRLF | `.gitattributes` 强制 `*.sh` LF |
| `devicetable-offsets.c` 缺 `generated/autoconf.h` | 内核树未跑 `syncconfig` | `sync_remote_build.sh` 自动 `make include/generated/autoconf.h` |
| `file2alias.c` 编译失败 | stddef 重入 + linux/stddef.h 误解析 | 移除 include-once；GCC include 优先 |
| file2alias IR | DEF_FIELD typeof / devtable 非常量初始化 | `type_of_member` + 函数指针静态初始化 |

## 远程 debug 记录（2026-07-16，GCC libiberty）

| 问题 | 根因 | 修复 |
|------|------|------|
| `#if 'A' == 0x41` 为假 | hex 被 ident 替换拆坏 | `replace_unknown_idents` 保留 `0x`/`0b` |
| `argv.c` 栈溢出 | 宏参数展开丢涂蓝集 | 继承 disabled + 深度上限 |
| `limits.h` / `lrealpath` | 无 `#include_next` | 实现并优先匹配指令名 |
| `argv.c` `_Generic` 解析失败 | 三元 else bp=0 吞逗号 | else 改为 `parse_expr_bp(2)` |

## 已实现模块（2026-07-15 扩展）

| 特性 | 说明 |
|------|------|
| BTF struct 探测 | `build_btf_probe()`：int + `dcc_probe` 结构体 |
| eh_frame FDE | 每函数 PC+size 生成 FDE；ORC 节写入 PC 列表 |
| LTO 链接 | `src/link.rs`：`ld -r` + `objcopy --add-section .llvm.lto` |
| CLI 多输入 | `-r a.o b.o -o merged.o` 链接模式 |
| 验收脚本 | prepare0 / pahole / objtool 三路回归 |

## 远程基线

- 工作区：`~/dcc`（亦见历史 `~/dcc-agent-linux66`）
- 同步构建：`scripts/sync_remote_build.sh`（含 prepare0/pahole/objtool 回归）
- 内核树：`~/linux-6.6`，需已有 `.config`
- GCC 子集：`~/gcc-src` + `scripts/remote_libiberty_probe.sh`
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
