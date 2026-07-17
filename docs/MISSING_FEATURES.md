# DCC Linux 6.6 缺失特性（按自动化证据）

> 最后更新：2026-07-17 — 以 `scripts/sync_remote_build.sh`、
> `scripts/kernel_full_check.sh`、`scripts/prepare0_check.sh`、
> `scripts/pahole_regress.sh`、`scripts/objtool_regress.sh`、
> `scripts/modpost_link.sh`、`scripts/vmlinux_regress.sh`、
> `scripts/archive/remote_libiberty_probe.sh`
> 与远程 Linux 6.6 / GCC libiberty 日志为准。

## 已通过的门禁

| 门禁 | 证据 |
|------|------|
| typed IR + 严格语义 | `cargo test` integration（static_assert / unknown typedef / int128）+ `tests/errors.rs` snapshot（E1003–E1008 等） |
| SysV va_list（GP） | `examples/test_p4_va_list.c` |
| asm goto + computed goto | `examples/test_asm_goto.c` |
| 自定义 ELF section / WEAK / HIDDEN | `examples/test_p2_elf_attrs.c` + readelf 断言 |
| __int128 / register asm / 全局符号 | `examples/test_p1_features.c` |
| 预处理 forced-include 注释安全 | `tests/fixtures/preproc_comment_force.h` |
| mod/devicetable 生成 TU | `devicetable-offsets.c` 直接 `-S` 输出 50+ `SIZE_*` 行 |
| DWARF + BTF（含 struct） | `-g` 生成 `.debug_*` + `.BTF`（含 `dcc_probe` STRUCT 类型） |
| objtool / ORC 帧信息 | `-g`/`-fno-omit-frame-pointer` 生成 CIE+FDE `.eh_frame`、`.orc_unwind_ip` |
| KASAN/UBSAN/KCSAN/KCOV 插桩（初版） | `-fsanitize=*`：TU init 一次 + 每函数入口桩 + `.dcc.san_info` v1 |
| 跨 TU LTO | `-flto` 生成 `.llvm.lto`；`-r` 多 `.o` 经 `ld -r` + objcopy 合并摘要 |
| LTO dead symbol 剔除 | `-ffunction-sections`（`-flto` 默认）+ `ld --gc-sections` + objcopy strip |
| `-S` 汇编输出 | lock/SSE 前缀；自定义节按 `.section` 分组 |
| modpost `file2alias.c` | 单 TU `-c` 生成 `file2alias.o` |
| prepare0 窄目标 | `scripts/prepare0_check.sh`（devicetable / bounds / file2alias） |
| pahole / objtool 单 TU 回归 | `scripts/pahole_regress.sh`、`scripts/objtool_regress.sh` |
| modpost 链接 / Rust fixdep | `scripts/modpost_link.sh` + `src/bin/fixdep.rs` |
| prepare0 全链路 | `scripts/prepare0_full.sh`（远程 PASS） |
| QEMU / vmlinux 回归脚本 | `scripts/qemu_boot_matrix.sh`、`scripts/vmlinux_regress.sh` |
| 内核全树编排 | `scripts/kernel_full_check.sh`（缺产物时 SKIP） |
| `#if` 十六进制字面量 | `preproc` 单测 `if_hex_*`（原 `0x41` 被拆坏） |
| `#include_next` | `preproc` 单测 + glibc `<limits.h>` |
| 宏展开涂蓝 / 防栈溢出 | `nested_function_macro_*`；远程 `argv.c` 不再 abort |
| `_Generic` + 三元控制表达式 | `tests/fixtures/generic_ternary.c`；远程 `libiberty/argv.c` |
| `_Generic` 指针精确匹配 | `tests/fixtures/generic_pointer.c` |
| 内联 asm 约束 | `examples/test_asm_constraints.c` |
| SysV va_list SSE | `examples/test_p4_va_list.c`（GP + float/double） |
| Mini x86 asm 局部/C 标签 | `tests/fixtures/asm_local_label.c`、`asm_label_insn.c`（`N:`/`N:insn`/`found:`） |
| Mini x86 asm 位操作 / 双精度移位 | codegen `btl/bts/btr`、`shld/shrd`（内核 paravirt 等） |
| 全局 SIMD 向量复制 | `tests/fixtures/vector_global*.c`；全局 init + 间接 load + 逐元素 store |
| x86_64 codegen 扩展 | 向量全局 init、复数四则/Cast、object_size fold、apply/return 子集、asm mem 操作数 |
| aarch64 typed IR | hello `.o`、整数 return/二元表达式；见 `aarch64_ir_add_cross_compile` |
| 诊断 snapshot 格式锁定 | `tests/errors.rs` schema + `diagnostic_format.stderr.snap`（V1） |
| `struct __attribute__((__may_alias__))` | `tests/fixtures/may_alias_struct.c` |
| 宏 `#` 字符串化换行折叠 | `preproc` 单测 `stringify_collapses_*`；`NOFAIL`/`modpost.c` |

## 阻塞级（Linux 6.6 全树）

| 特性 | 当前状态 | 说明 |
|------|----------|------|
| prepare0 全链路 `make prepare0` | ✅ 脚本等价 | 用 `prepare0_full.sh`；`make prepare0 CC=dcc` 豁免（kconfig 宿主 C） |
| 全树 `make CC=dcc` | ⏳ 待远程首跑 | 推荐 `HOSTCC=gcc` 混合；Rust fixdep 已替代 C fixdep |
| modpost.c / sumversion.c（宿主） | ⏳ 部分过编 | `NOFAIL`/`#` 字符串化、`__may_alias__` 已修；仍剩 duplicate typedef / `bool` 等 |
| QEMU 启动矩阵 | ⚠️ 脚本就绪 | 无 bzImage 时 SKIP；需步骤 6 产物 |
| vmlinux pahole / objtool | ⚠️ 脚本就绪 | 无 vmlinux 时 SKIP；单 TU 回归已通过 |

## 短期：GCC libiberty（纯 C）

详见 `docs/GCC_BUILD_GUIDE.md`。远程当前（2026-07-16）：

| 组件 | 结果 |
|------|------|
| libiberty 单 TU | **125 OK / 0 FAIL / 4 SKIP** |
| libiberty `dcc -r` 链接 | ✅ ~1MB 合并 `.o` |
| fixincludes | **6 OK / 0 FAIL / 1 SKIP** |
| libdecnumber | 11 SKIP（include 单元） |

| 缺口 | 影响 |
|------|------|
| 无 C++ | 无法编 GCC 主体 / 完整 bootstrap |
| `_Generic` 指针精确匹配 | ✅ 按 pointee 匹配；void* 与对象指针兼容 |
| libdecnumber 组合编译 | 需 DECPMAX 宏驱动的多文件 include，未纳入 probe |
| 内联 asm 约束 | ✅ `"r"`/`"m"`/`"=r"`/`+m`、`memory` clobber |
| SysV va_list SSE | ✅ GP + XMM0–7 保存区；float/double/long double va_arg |

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
| 浮点比较/`&&`/`\|\|` | 结果类型标成 float | 比较与逻辑结果恒为 int |
| 宏参替换破坏字符串 | `replace_ident` 进字面量 | 跳过 `"`/`'` |
| `##` 后再替换 → `&&di` | paste 全局替换 | 只替换 ## 两侧 token |
| `TRANSLATE(*d)` → `(**d)` | 无 `##` 的 `handle_paste` 仍替参 + 第三步多遍扫描 | 无 `##` 早退 + `replace_all_params` 单次不透明替换 |
| `regex.c` 完整文件仍 `(**d)` | V2 include 缓存保留旧展开 | 缓存 bump V3；远程 **125/0** |
| `char const *` / `char const **` | `const` 在 `*` 前未跳过 | `finish_type` 限定符顺序 |
| libiberty 链接 multiple definition | extern 声明 + 定义双份 symtab | 符号收集去重 |
| `ld -r --gc-sections` | bfd 缺 `-u` 根符号 | `gc_sections_root` + `-u` |
| `struct T *a, *b` 变 `T**` | 说明符吃掉 `*` | `parse_decl_specifiers` + 多声明符 |
| 全局 `&arr[i]` | 初值校验过严 | 静态初值 + rela addend |
| 前向 struct 成员 | 空 fields | 空时查 `struct_defs` |

## 远程 debug 记录（2026-07-16，asm-offsets / prepare0）

| 问题 | 根因 | 修复 |
|------|------|------|
| stmt_expr 后 `()` 未解析 | 语句表达式缺后缀链 | `parse_postfix_suffix()` |
| `__typeof__(struct T)` | `(` 后误用 `is_peek_type_start` | 改为 `is_type_start()` |
| `section("a" "b")` | 属性只读一个 StringLit | `parse_concatenated_string_lit()` |
| `__aligned__((expr))` | 只支持纯数字 | 括号深度跳过完整参数 |
| asm 试探 skip 未恢复 lexer | bookmark/restore + 仅 asm goto 试探 | `probe_asm_goto()` |
| 位域 `a:8, b:4` | `parse_expr()` 吞逗号 | `parse_expr_bp(2)` |
| struct `{ lock; ; }` | 空字段非法 | `parse_fields` 跳过 lone `;` |
| `__auto_type` / typedef `(fn)(...)` | 未支持 GNU 分支 | 新增解析路径 |
| C11 atomic 预置宏 | 与内核 atomic-instrumented 冲突 | 删除 preproc 预置宏 |
| `_Static_assert(offsetof(...))` | const_eval 无 Offsetof | `offsetof_member()` + find_field |
| `DIV_ROUND_UP` 未展开 | object-like 别名后 `(args)` 未重扫描 | preproc 条件 tail_call 重扫描; 缓存 V5 |
| `tags[3][EXPR]` | 结构体字段只解析一维 `[...]` | while LBracket + const_eval 数组界 |
| `void __attr__ *fn()` | void 后属性残留 | `skip_attributes` 后再 `parse_ptr_suffix` |
| 语句表达式 `q * n_` | `is_decl_start` 误判 | ident 须为类型名；补 `__auto_type` |
| `_DECLARE_STATIC_KEY_0(x)` | 预处理二次宏残留 | `parse_static_key_decl()` 语义补全 |
| `ret ?: -7` | 未实现 GNU 省略三元 | 中间缺省则 then_=cond |
| `current_thread_info()` 未展开 | include 缓存零参宏误读为 `[""]` | load 过滤空 param；缓存 V6 |
| percpu 三元 `*` 解引用 E1004 | `Cond` 固定 int 类型 | 分支为指针时保留指针类型 |
| codegen panic `6f` | 内联 asm 缺 GNU 局部数字标签 | `AsmLocalLabels` N:/Nf/Nb |
| `-O2` 后 `goto found` panic | DCE 删除 `return` 后 C 标签 | `opt.rs` 保留 `Stmt::Label` |
| 同行 `1:jmp` / `found: ret` | 整行当助记符 `1:jmp` | `expand_label_insn_lines` 拆分 |
| `btl`/`shld` 等 nop 降级 | mini-asm 缺编码 | `emit_asm_bit` / `emit_asm_shld_shrd` |
| modpost `Invalid token`（5560） | `#ptr` 字符串化含换行 | `stringify_arg` 折叠空白 |
| sumversion `__may_alias__` | struct 名前的类型属性 | `parse_type_mode` 后 `skip_attributes` |

**当前 asm-offsets 状态**：解析 **0**、语义 **0**；`-O2` 不再 panic `unresolved label: found`；
全树 `-O2 -c` 仍可能遇 paravirt 类型冲突等后续阻塞。验收脚本：
`scripts/test_asm_offsets.py`。

## 已实现模块（2026-07-16 扩展）

| 特性 | 说明 |
|------|------|
| jump_label 静态 key 声明 | `parse_static_key_decl()`：`_DECLARE_STATIC_KEY_0/1` 及 `IS_ENABLED` 残留 → `extern struct static_key_{false,true}` |
| Typed IR 语义校验 | `ir.rs`：`is_static_initializer` 扩展；float/vector/complex 二元/一元运算与 codegen 对齐 |
| BTF struct 探测 | `build_btf_probe()`：int + `dcc_probe` 结构体 |
| eh_frame FDE | 每函数 PC+size 生成 FDE；ORC 节写入 PC 列表 |
| LTO 链接 + gc | `src/link.rs`：`ld -r --gc-sections` + `.llvm.lto` 注入 + dead strip |
| per-function section | `-ffunction-sections` / `-flto` 默认 `.text.<name>` |
| rela 引用标记 | `elf::referenced_symbol_names` → `lto::mark_references_from_objects` |
| CLI 多输入 | `-r a.o [b.o…] -o merged.o` 链接模式；多 `.c` 须 `-c` 后用 gcc 或 `-r` |
| 链接/LTO 模块（非自研） | `link.rs`+`lto.rs`：host `ld/objcopy`、`.llvm.lto` 摘要 + gc-sections；无 IR 跨 TU 优化 |
| 验收脚本 | prepare0 / pahole / objtool / modpost / vmlinux / kernel_full_check |

## 远程基线

- 工作区：`~/dcc`（亦见历史 `~/dcc-agent-linux66`）
- 同步构建：`scripts/sync_remote_build.sh`（含 prepare0/pahole/objtool/kernel_full_check）
- 内核树：`~/linux-6.6`，需已有 `.config`
- GCC 子集：`~/gcc-src` + `scripts/archive/remote_libiberty_probe.sh`
- 内核全树：`scripts/kernel_full_check.sh`（modpost / QEMU / vmlinux，缺产物 SKIP）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` + bzImage）
