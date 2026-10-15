# DCC 缺失特性与已知限制

> 最后更新：2026-10-02 — 基于当前源码、测试与脚本审计
>
> 已支持特性详见 `docs/support_features.md`。本文件只记录仍不支持、
> 明确降级、仍为 stub/no-op，或缺少测试契约的项目。

## 后端 codegen 限制

> 非 x86 后端 mini-assembler 已支持常见指令 (-c 路径编码机器码), 详见 `support_features.md`

| 限制 | 说明 |
|------|------|
| `-falign-jumps=N` / `-falign-loops=N` (N>1) | DCC 不执行代码段对齐；`=1` 等价于不对齐，静默接受；其他值发出警告 |
| `-mpreferred-stack-boundary=N` (N≠2) | DCC 不调整栈对齐；`=2` 为内核默认，静默接受；其他值发出警告 |
| `-fno-plt` | 接受但无操作；内核不使用 PLT，DCC codegen 不生成 PLT 入口 |
| `-ftrampolines` / `-fno-trampolines` | 接受但无操作；嵌套函数直接调用通过 `__builtin_call_with_static_chain` 支持；嵌套函数取地址时生成栈上 trampoline (设置 r10=父帧指针后跳转)，用户需确保栈可执行 (mprotect PROT_EXEC) 或通过 `-z execstack` 链接 |
| 优化器 `-fstrict-aliasing` | **无效果，但会明说**。优化器不做基于类型标签的别名假设（TBAA）：原先那份别名分析因「结果从入口到管线全部被丢弃」已删（判据见 `src/ssa/passes.rs` 的 P4/P13 删除说明与 `tests/integration/c_lang.rs` 的 `-fstrict-aliasing` 用例），至今没有接上真消费者。因为产物永远等于「保守 aliasing」那一侧：`-fno-strict-aliasing` 静默接受（它要求的本来就是现状，内核构建全部传这个），而 `-fstrict-aliasing` 会发一条 `dcc: warning: ignoring '-fstrict-aliasing' …`，不默假做了。判据：`fstrict_aliasing_is_reported_as_inert_but_fno_is_silent`（三个方向：警告 / 不警告 / 不传也不警告）。真正的 TBAA 仍是开放项。|
| x86 段寄存器限定符 `__seg_gs` / `__seg_fs`（**指针 pointee** 位置） | 已实现且逐形核对：解引用、下标、成员、复合赋值/自增自减、cast、typedef、typeof、局部/形参/函数返回类型均发 `%gs:`/`%fs:`，与 gcc 一致。| 
| x86 段寄存器限定符加在**对象本体**上 | 已实现：`__seg_gs int gv;` 后读 / 写 / 下标 / 成员 / 复合赋值 / 自增自减 / 聚合拷贝均发 `%gs:`/`%fs:`，与 gcc 同形。做法是「符号→段」表（`Codegen::global_seg`，键是**过 `resolve_global_key` 之后**的符号键，函数体内 `static` 靠 mangled 名）+ `emit_global_load` / `emit_global_store` 两个下发口。两个**刻意不做**的形状：取址（LEA）不发前缀（GAS：`segment override on 'lea' is ineffectual`，地址仍不含段基），以及段与 TLS 不合并（前者改指令字节、后者改重定位种类）。判据：`seg_qualifier_on_object_body_emits_segment_prefixes`（正向 24 项按前缀**笔数下限**、fs 5 项、反向对照 13 项、两档都跑 + gcc 四档执行级对拍）。历史与未做的诊断未单独成文（本行即记录）。|
| IFUNC（`__attribute__((ifunc))` / `target_clones`）与动态链接 | **已真修**（resolver 从内置链接器搬回 codegen）。`.o` 里 `compute` 是 `STT_GNU_IFUNC` 且与 `compute.resolver` **同址同尺寸**，resolver 体真做 CPUID（`call __dcc_cpu_indicator_init` + `testl $位,__dcc_cpu_model(%rip)` + 两个 `lea <变体>` + `ret`）⇒ 宿主 `ld` 原生可处：`file` 报 `dynamically linked`、`readelf -d` 有 `NEEDED libc.so.6`、`readelf -rW` 有 `R_X86_64_IRELATIVE`，运行与 gcc 逐字一致。`driver.rs` 里「含 IFUNC 就改道内置链接器」的分支与 `elf_has_ifunc_symbols` 已删 —— 静默降级那个形状不再存在。`__builtin_cpu_supports` / `__builtin_cpu_is` 同时从恒假桩变成真实现（逐名与 gcc 对照；dcc 给 0/1、gcc 给 libgcc 位值，文档只保证 non-zero），且检测体自包含（`nm -u` 不引用 `__cpu_model`/`__cpu_indicator_init`）。特性位/优先级/分类表只有一份，在 `semantics::cpu_isa`，`link/target_clones.rs` 为薄转发（仍服务 `.dcc.ifunc`/外部 `.o`）。仍走内置链接器的静态路径不做运行时派发（绑 default），但它三行齐报、不静默。历史与实测边界本行为准。判据：`target_clones_links_dynamically_with_codegen_resolver`（原三态哨兵已翻）、`target_clones_runtime_resolves_to_non_default_variant`、`attr_target_clones_cpu_supports_matrix_matches_gcc`、`attr_target_clones_cpu_is_model_names_match_gcc`、`table_agrees_with_host_detector`、`target_clones_libc_links_via_host_cc_and_builtin_failure_is_attributed`。|
| `target_clones` 各变体的**机器码目前相同** | dcc 没有 per-function 的 ISA codegen：`FuncDecl` 带 `target_clones` 元信息，但 ISA 上下文不传到指令选择层，所以 `compute.sse4_2` 与 `compute.default` 逐字节一样（已按 `st_size` 全量比过四组）。“选中了哪个变体”因此只能由地址差与手写 `ifunc` 观察。不属于上一行那条链接缺陷（它修的是链接方式），是独立一项。|
| 地址空间的**诊断** | gcc 对 `int * __seg_gs p`（限定符在 `*` 之后）、自动变量带段、`__seg_gs` 指针与普通指针互相赋值均报错；dcc 全盘接受（比 gcc 宽松）。不会错值，但也不是契约对齐。|


## 链接器 / 对象文件限制

| 限制 | 说明 |
|------|------|
| COFF 动态链接 | COFF 目标文件可生成、解析并链接为 PE 可执行文件；但不支持 DLL 导入表、动态链接等高级 PE 特性 |

## 交叉编译 / 缺失平台

| 缺失平台 | 说明 |
|----------|------|
| Darwin / Mach-O | 无 Mach-O 对象与链接支持 |
| WASM | 无 wasm32/wasm64 codegen |
| AVR / MSP430 / Xtensa | 无 embedded backend |
| PowerPC / LoongArch64 | 无对应 codegen/link 路径 |

## 自举

| 步骤 | 状态 |
|------|------|
| 步骤 1: 自举基础设施 | ✅ 完成 — `src/bootstrap.rs` 三层对比 (byte/struct/semantic)，`--bootstrap-compare=<ref.o>` CLI |
| 步骤 2: `dcc0 → dcc1` 编译验证 | ✅ 完成 — `bootstrap_selftest.c` (14 组测试) + `scripts/bootstrap.sh` 4 阶段流程 |
| 步骤 3: `dcc1 → dcc2` 确定性验证 | ✅ 完成 — 字节级 + 语义级对比通过；多文件编译一致性验证通过 |

## 本地基线

- 工作区：`~/dcc`
- 编译流程：`cargo build --release` + `bash scripts/test_all.sh`
- 内核树：`~/linux-7.1`，需已有 `.config`（QEMU 启动需启用
  `CONFIG_SERIAL_8250_CONSOLE`）
- GCC 子集：`~/gcc-src` + `scripts/libiberty_combined.sh`
  （由 `cargo test --test integration` 驱动）
- 内核全树：`scripts/kernel_full_check.sh`（默认保留缺产物 SKIP；
  `STRICT_FULL_BUILD=1` 要求 vmlinux / bzImage / modpost / pahole /
  objtool / QEMU boot 全部硬通过）
- QEMU：`scripts/qemu_boot_matrix.sh`（需 `qemu-system-x86_64` +
  bzImage；2026-08-27 验证通过，串口启动日志正常）
