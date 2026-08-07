# DCC 未实现模块综合清单

> 最后更新：2026-07-21
> 范围：`src/` 全量源码 + `docs/` 现有文档
> 目的：把"代码里能看到、但还没做完整 / 还没真做 / 仍然走桩"的模块统一记录，
> 方便后续 M12+ 排期，避免"忘了还有这块没补"。
> 维护约定：每完成一个 M 里程碑后, 在对应节加 `🟢 已实装` 标记 + 简述
> 改动要点 (含回归测试), M 之前标记为 `🟠 简化` 或 `🔴 完全没有`。
> 调试环境：当前在 Ubuntu 本地调试 (已安装, 磁盘充足),
> 一切以本地 `cargo test` / `cargo build` 为准.
> 同步: 本节 8.2 状态以 `tests/fixtures/errors/*.stderr.snap` 与
> `tests/errors.rs::diag_*` 实际产物为准。

---

## 0. 速览（按"距离能用"距离排序）

| 状态 | 含义 | 数量（粗略） |
|------|------|--------------|
| 🟢 已实装并有测试 | `cargo test` 通过 | 见各里程碑 |
| 🟡 已解析/已降级，行为简化 | 走 no-op / placeholder / 不严格 C 语义 | ~15 |
| 🟠 桩已交付，运行时未接 | 框架代码在但实际不工作 | ~10 |
| 🔴 完全没有 | 解析或后端未实现 | ~20 |

下面按 **子系统** 列出，所有标 🟠 / 🔴 的项都是 M12+ 的 backlog。

---

## 1. 后端 codegen 未实现的指令/操作数

### 1.1 x86_64 mini-asm 指令子集

x86_64 codegen 对 GNU 内联 asm 的 mini-asm 汇编器已覆盖常用指令，但仍有不少
"warning: unsupported … operand pattern, emitted as nop" 路径（见
`src/codegen/x86_64.rs` 第 8380–9292 行的 `eprintln!` 警告簇）。

🟠 已识别但降级为 nop 的操作数模式（典型）：

| 类别 | 缺失模式 | 触发样例 | 状态 |
|------|----------|----------|------|
| `mov` | 段寄存器 (`%gs:`, `%fs:`)、imm64 内存目标、3 立即数 | 内核 `mov %gs:...` / `movabs` 内存 | ✅ M12 mini-asm |
| `push` / `pop` | `push imm32` 立即数大于 8 位、`push %gs` | paravirt patch 序列 | ✅ M12 mini-asm |
| `lea` | 32 位 disp + 索引、sib 八种组合 | 部分 per-cpu 访问 | ✅ 已有 |
| ALU | `add $imm8, %r13` 立即数过大、`sub %r13d, ...` 8/16 位 `op imm8` | — | ✅ M12 mini-asm |
| `test` | imm 内存目标、`test $imm, %al` | — | ✅ M12 mini-asm |
| 移位 | `shl $cl, %rsp`（cl 变体之外） | 内核 paravirt | ✅ M12 mini-asm |
| `imul2/imul3` | 三操作数内存目标 | 旧 ABI | 🟡 部分 |
| `cmpxchg` | 非 rax 隐式操作数 | 某些 RCU 路径 | ✅ M12 mini-asm |
| `xadd` | 内存 + 寄存器（隐式 LOCK） | 引用计数 fallback | ✅ M12 mini-asm |
| `jmp` / `jcc` / `call` | 远跳转 / 远调用 / 绝对立即 | 实模式 / 启动代码 | ✅ M12 mini-asm（`*sym` 走 RIP-relative + `jmp *%rax`） |
| `setcc` | 内存目标 AH/BH/CH/DH | — | ✅ M12 mini-asm |
| `movzx` / `movsx` | 8→32 / 16→32 内存源 | — | ✅ M12 mini-asm |
| `popcnt` | 内存源 | SSE4.2 | ✅ M12 mini-asm |
| `bswap` | 16 位操作数 | — | 🟡 16 位不支持（gcc 同） |
| `movnti` / `movntdq` / `movntps` / `movntpd` | 内存源缺失 | NT store | ✅ M12 mini-asm |
| `prefetch` | 内存源 / `(%rip)` 寻址 | — | ✅ M12 mini-asm |
| `int` / `sys` / `sysenter` / `sysexit` / `vmlaunch` / `vmresume` | 完全 stub | 启动代码 / VMM | ✅ M12 mini-asm（除 `sys` 64 位） |
| `swapgs` / `invlpg` / `wbinvd` / `cpuid` / `rdtsc` | 缺编码 | 内核入口 | ✅ M12 mini-asm |
| `ud2` | 占位 panic | — | ✅ M12 mini-asm |
| `sse mov` / `sse arith` | 大量 xmm 内存模式 | SIMD 内联 | 🟡 部分 |

✅ **M13 已完成**（见 `docs/architecture.md` M13 章节）— x86_64 mini-asm
SIMD 指令子集完整实装：

| 扩展 | 状态 | 覆盖 |
|------|------|------|
| AVX/AVX2/AVX-512 | ✅ | `v*` 前缀、`vzeroupper`/`vzeroall`、`vextractf128/i128`、`vpinsr*`/`vpextr*`/`vextractps/pd`、EVEX `_z` 简化路径 |
| SSE3–SSE4.2 | ✅ | `pshufb`/`pmulhrsw`/`pinsr*`/`pextr*`/`extractps`/`insertps`/`round*`/`crc32*` 等 30+ 条 |
| AES-NI / SHA | ✅ | `aesenc/dec*`、`sha*rnds*`/`sha*msg*` 共 13 条 |
| FMA / F16C | ✅ | 32 个 FMA 4 操作数变体 + `vcvtph2ps`/`vcvtps2ph` |
| TSX | ✅ | `xbegin`/`xend`/`xabort`/`xtest` |
| AMX / AVX-VNNI | ✅ | `ldtilecfg`/`sttilecfg`/`tilezero`/`tileloadd`/`tilestored` + 4× `tdp*` + `vpdp*`；`tmm0–7` 寄存器解析 |
| 寄存器名 | ✅ | `xmm`/`ymm`/`zmm`/`tmm`/`k0–7` 均可解析 |

测试：`tests/fixtures/c/asm_avx.c`（含 AMX/VPINSR/EVEX/TSX xabort）+
`asm_avx_aes_fma_tsx_compiles_without_nop_fallback` 本地 PASS。

**M13.2 增量（2026-07-21）**：修复 `pinsr*`/`pextr*`/`extractps`
对 GPR/内存操作数的编码（此前 inner 条件不匹配会静默漏发字节）；
新增 `vpinsrb/d/q`、`vpextrb/d/q`、`vextractps/pd`；`parse_asm_reg`
支持 `ymm`/`zmm`/`tmm`/`k`；AMX `tileloadd`/`tilestored` 兼容
`(tmm, mem)` 与 `(mem, tmm)` 两种 AT&T 顺序。

### 1.2 x86_64 codegen 表达式层

> **M13.1 已完成**（见 `docs/architecture.md` M13.1 章节）：float 取模软件实装
> （divss/divsd + cvttsX2si + cvtsi2sX + sub/mul，无 libc 依赖）、int128 `&&`/`||`
> 合并 rax:rdx 后再 test（避免高 1 低 0 误判 0）、asm `=&x` early-clobber
> 独立 XMM 池（不再复用 GP 集合判定冲突）、asm goto 输出寄存器写回阶段支持
> 落入不同 XMM；float/跨函数 LEA/rip-relative `[rip+disp32]` 通过既有
> `float_refs`/`pending_data_rels` 重定位 + patch 路径已稳定支撑。
>
> **M21 增量（2026-07-21）**：asm `%0` 在约束 `"i"` 下正确展开
> 为带 `$` 前缀的 AT&T 立即数（与 GCC 行为一致），Kbuild
> `devicetable-offsets.c` 的 `DEFINE(SIZE_..., sizeof(struct ...))`
> 宏能正确产出 `.ascii "->SIZE_usb_device_id $8 sizeof(struct
> usb_device_id)"` 行；`kbuild_define_emits_ascii_in_asm_output`
> 集成测试从错误预期 `8` 修正为 `$8` 后 PASS。

历史 stub（已全部解决）：

| 模块 | 文件:行 | 现状（M13.1 后） |
|------|---------|------------------|
| 浮点字面量 rodata 解析 | `x86_64.rs:3194` | `v.to_le_bytes()` 直接写入 `.rodata`，f32/double IEEE 754 精确，movss/movsd 加载，复杂字面量无精度偏差 |
| 跨函数 LEA | `x86_64.rs:5157` | `[r13+off]` 用 `emit_lea_r13`；RIP-relative 走 `pending_data_rels` patch；全局数组下标新增 `emit_load_global_indexed` 单条 `[rip+disp32+idx*scale]` |
| `gen_float_binop` 不支持运算 | `x86_64.rs:4294` | float/double 取模软件实装：cvttsX2si + cvtsi2sX + mul + sub，零 libc；`tests/fixtures/c/float_mod.c` |
| XMM 临时槽 | `x86_64.rs:7550` | `prepare_asm_xmm_binding` 加载 float 到 xmm；`writeback_asm_operand` 按 `xmm_width` 写回 xmm8–15；`input_xmm_set` + early-clobber `xmm_pool` |
| int128 binary 逻辑 | `x86_64.rs:2577` | `Expr::Binop` 优先 `&&`/`\|\|`，`or rdx, dst` 合并高 64 位；`tests/fixtures/c/int128_log.c` |

### 1.3 AArch64 后端

✅ M8 已实装（2026-07-21）：

| 模块 | 状态 | 说明 |
|------|------|------|
| `emit_bl_placeholder` | ✅ 已完善 | 增加 ±128MiB 范围校验 (`bl_offset_in_range`) + `debug_assert` |
| `emit_adr_placeholder` | ✅ 已完善 | 增加 ±1MiB 范围校验 + ADRP+ADD 退化路径 + `emit_adr_large` 双指令序列 |
| `emit_ldr_s0_u32_placeholder` | ✅ 已完善 | 增加 ±1MiB 范围校验 + 对齐检查 |
| HFA（同构浮点聚合） | ✅ 已实装 | `classify_hfa` + `load_hfa_to_vregs` + `store_hfa_from_vregs` + `gen_hfa_expr` |
| `>16B` struct 返回 sret | ✅ 已实装 | `memcpy_sp_to_x0` 逐块拷贝 + 调用方 `load_w0` 取结果 |
| eh_frame / ORC / sanitizer 节 | ✅ 已实装 | `mod.rs` 中 AArch64 分支已发射 `.eh_frame` + `.orc_unwind_ip` + `.dcc.san_info` |
| DWARF / BTF | ✅ 已实装 | `build_dwarf_full` 用 `AArch64` 架构选项 + `.BTF` 节 |
| 谓词指令（CCMP / CSEL / CSINC） | ✅ 已实装 | `emit_ccmp_w0_w1` + `emit_ccmp_w0_imm` + `emit_csel_w0_w0_w1` + `emit_csinc` + `emit_csneg` |
| 复杂寻址模式 | ✅ 已实装 | 后置索引 (`emit_str/ldr_post_index`) + 前置索引 (`emit_str/ldr_pre_index`) + 扩展寄存器 (`emit_ldr/str_ext_reg`) |
| 浮点比较 (`fcc*`) | ✅ 已实装 | `fcmp_s0_s1` + `fcmp_d0_d1` + `fcmp_s0/d0_zero` + `emit_fccmp_s0_s1_ne` + `emit_fccmp_d0_d1_ne` |
| 缓存维护指令 (`dc *` / `ic *`) | ✅ 已实装 | `emit_dc_cvac/civac/cvau/ivac` + `emit_ic_ivau/iallu` + `emit_dsb_ish/sy` + `emit_isb` + `emit_cache_sync_range` (循环结构已修复) |

🟠 仍待完善：

| 模块 | 现状 |
|------|------|
| NEON 浮点聚合传参 > 4 元素 | 仅支持 1–4 元素 HFA |
| 大结构体 > 16B 嵌套返回 | 仅单层 sret，未验证多层嵌套调用链 |
| TLS 重定位 (`R_AARCH64_TLSDESC_*`) | 仅基础 `tpoff` 模型 |

### 1.4 RISC-V64 后端

✅ M8 已实装（2026-07-21）：

| 模块 | 状态 | 说明 |
|------|------|------|
| `emit_jal_placeholder` | ✅ 已完善 | 增加 ±1MiB 范围校验 (`jal_offset_in_range`) + `debug_assert` |
| HFA（同构浮点聚合） | ✅ 已实装 | `classify_rv_hfa` + `HfaInfo` 分类，支持 float/double 1–4 元素 |
| `>16B` struct 返回 sret | ✅ 已实装 | `memcpy_sp_to_a0` 逐块拷贝 + 调用方 sret 槽 |
| eh_frame / ORC / sanitizer 节 | ✅ 已实装 | `mod.rs` 中 RISC-V 分支已发射 `.eh_frame` + `.orc_unwind_ip` + `.dcc.san_info` |
| DWARF / BTF | ✅ 已实装 | `build_dwarf_full` 用 `RiscV64` 架构选项 + `.BTF` 节 |
| 压缩指令 (C 扩展) | ✅ 已实装 | `emit_c_addi/c_nop/c_li/c_lw/c_sw/c_j/c_beqz` 16-bit 编码 |
| FP 扩展子集 (D 扩展) | ✅ 已实装 | `emit_fld/fsd` + `fadd/fsub/fmul/fdiv.d` + `fcvt.d.w/l/s` + `fmv.d.x/x.d` + `fsgnj.d` |
| 向量扩展 (V) 基础 | ✅ 已实装 | `emit_vsetvli/vsetivli` + `vle32/vse32.v` + `vadd/vmul.vv` + `vredsum.vs` |
| 特权指令 / CSR 访问 | ✅ 已实装 | `emit_csrrw/csrrs/csrrc/csrrwi/csrrsi/csrrci` + `emit_mret/sret/wfi` + `sfence.vma` + `fence/fence.i` |

🟢 **M23 已完成**（2026-07-21）：

| 模块 | 状态 | 说明 |
|------|------|------|
| D 扩展完整 | ✅ | `emit_fsqrt_d/fmin_d/fmax_d/feq_d/flt_d/fle_d` 全部实装 |
| Zicsr CSR 全集合 | ✅ | 6 条 CSR 指令完整: CSRRW/CSRRS/CSRRC + CSRRWI/CSRRSI/CSRRCI |

🟠 仍待完善：

| 模块 | 现状 |
|------|------|
| RVV 完整指令集 | 仅基础 VSETVLI/LOAD/STORE/ADD/MUL/REDUCE，缺宽化/窄化/掩码/浮点向量 |
| 压缩指令自动选择 | 手动发射，未实现自动 4→2 字节压缩优化 |

### 1.5 MIPS / ARM / MIPS64

🟢 **后端已实装**（`Arch::has_backend() == true`）：

| Triple | 现状 | 备注 |
|--------|------|------|
| `mips-linux-gnu` | 完整后端 (ELF32, big-endian) | MIPS32 O32 ABI |
| `mips64-linux-gnuabi64` | 完整后端 (ELF64, big-endian) | MIPS64 N64 ABI，复用 mips.rs |
| `arm-linux-gnueabihf` / `arm-linux-gnueabi` | 完整后端 (ELF32, little-endian) | ARM AAPCS |

`codegen::emit()` 支持这三个架构，可产出 ET_REL `.o` 与 `-S` 汇编。

---

## 2. 链接器（`src/link/`）

### 2.1 已实装（见 `architecture.md` M11.3 章节）

✅ LTO 跨函数内联 / 常量传播 / 全局优化；`.gnu.hash` TIE；`.eh_frame_hdr` 搜索表；
IFUNC；`--exclude-libs`；`--build-id=sha1/uuid/md5`；`--no-undefined-version`；
AArch64+RISC-V 重定位；GOT64；TLS 重定位；PIE/Shared 输出；动态节；
多架构 `_start` 入口桩（`ensure_start_stub`：x86_64/AArch64/RISC-V64/ARM）。

> **M25 增量（2026-08-06）**：`_start` 桩从仅 x86_64 扩展为多架构感知。
> `Layout`/`LayoutBuilder` 新增 `arch` 字段，`run_builtin_linker` 中 `arch` 和
> `dynamic_linker` 路径改为按目标架构自动选择（`arch_from_target` /
> `dynamic_linker_path` 辅助函数），消除硬编码的 x86_64 依赖。新增 PIE 集成测试
> `pie_builtin_linker_return_42` 验证 PT_INTERP + _start 桩 + 动态重定位完整链路。

> **M21 增量（2026-07-21）**：修复 `ld -r -flto` 在现代 binutils 下
> 的 `--gc-sections requires a defined symbol root` 错误。新增
> `codegen::elf::global_symbol_names` 解析输入对象的 STB_GLOBAL /
> STB_WEAK 符号；`link::legacy::link_relocatable` 在 LTO 模式下为
> 每个全局符号追加 `-u <name>` 作 root，使 `--gc-sections` 不会
> 拒绝入口。受影响 4 个集成测试 (`flto_relocatable_link_merges_section`
> / `lto_single_object_relocatable_link` / `lto_link_strips_unreferenced_static`
> / `lto_summary_includes_function_size`) 全部 PASS。

### 2.2 未实装 / 简化

🟢 **M22 已完成**（2026-07-21）— 以下模块从 🟠/🔴 升级为已实装：

| 模块 | 状态 | 实现要点 |
|------|------|----------|
| `--no-undefined-version` | ✅ | `link/symver.rs::VersionTable` 完整版本表 + 严格检查 |
| AArch64/RISC-V/MIPS/ARM ELF 输出 | ✅ | `elf_exec.rs` 多架构包装函数，参数化 `machine: u16` |
| IFUNC 多版本 (target_clones) | ✅ | `link/target_clones.rs` 生成 STT_GNU_IFUNC + R_X86_64_IRELATIVE |
| LTO 4 pass 增强 | ✅ | `LtoConfig` 配置化 + Pass 7 no_semantic_interposition |
| `gc_sections_root` 自动选根 | ✅ | 无显式根时自动选所有 STB_GLOBAL/STB_WEAK 符号 |
| relocatable 模式 (.o → .o) | ✅ | `legacy.rs` 内置路径 `link_relocatable_builtin` |
| 共享库 lazy PLT | ✅ | `plt_got.rs` 完整 .plt.sec/.plt.got 生成 |
| BIND_NOW / `-z now` | ✅ | `DT_FLAGS=DF_BIND_NOW` + `DT_FLAGS_1=DF_1_NOW` |
| `-z relro`/`-z now`/`-z noexecstack` | ✅ | `PT_GNU_RELRO` 段 + `PT_GNU_STACK` PF_X 控制 |
| `RPATH` / `RUNPATH` | ✅ | `DT_RPATH` (23) / `DT_RUNPATH` (29) 动态项 |
| Symbol versioning `.symver` | ✅ | `VersionTable` + `.gnu.version` + `.gnu.version_d` |
| `-flto` legacy `.llvm.lto` | ✅ | 旧代码已删除，仅保留 `.dcc.lto` DBC 位码 |
| `__dso_handle`/`__cxa_atexit`/`atexit` | ✅ | `fini_dispatch.rs::FiniArrayBuilder` 按 priority 排序 |
| Compress debug sections (zlib) | ✅ | `link_opts.rs::compress_debug_sections` + miniz_oxide |
| `SHF_GNU_RETAIN` 标记 | ✅ | 元数据节自动标记 0x200000，gc-sections 保留 |
| `-z pack-relative-relocs` | ✅ | `SHT_GNU_RELR` 段生成 |
| `-z max-page-size` | ✅ | 解析 K/M/G 后缀，控制 PT_LOAD align |
| `-fno-semantic-interposition` | ✅ | LTO Pass 7 标记可直接访问的全局/函数 |
| `-fuse-ld=` 多链接器切换 | ✅ | `LinkerBackend` 枚举 + `resolve_linker_backend` |
| 自定义 dynamic linker 路径 | ✅ | `PT_INTERP` 程序头 + `.interp` 节 |
| Debug link (`--gdb-index`) | ✅ | 最小 `.gdb_index` 节占位（GDB 可识别） |

🟠 仍待完善：

| 模块 | 现状 |
|------|------|
| LTO IPA-SRA / PGO / 循环向量化 | 🟢 (M25) IPA-SRA (`src/ipa_sra.rs`) 和 PGO 集成 (`src/profile/pgo_integration.rs`) 已实装；循环向量化待后续 M |
| 运行时 dynamic loader | 完全依赖 host ld.so（设计决策，非缺失） |
| zstd 压缩 debug sections | 🟢 (M25) `compress_debug_sections(layout, "zstd")` 走 zstd::encode_all(级别 3) + ELF 压缩头 (ch_type=2 ELFCOMPRESS_ZSTD); CLI `-Wl,--compress-debug-sections=zstd` 已接受 |

---

## 3. Sanitizer 桩（`src/sanitizer.rs`）

🟢 AST 层全部通道已交付（KASAN / HWASAN / UBSAN / KCSAN / KCOV / Fuzzer）。

| Sanitizer | 实际能力 | 已知遗留 |
|-----------|----------|------|
| KASAN (`-fsanitize=address`) | TU init + `__asan_load1/2/4/8/16` / `__asan_storeN` 在 `Deref/Index/ArrowMember` 与对应 `Assign` 之前插入 | `*p = v` store 操作已验证工作正常 |
| HWASAN (`-fsanitize=hwaddress`) | 同 KASAN 路径但使用 `__hwasan_loadN` / `__hwasan_storeN` 函数名 | ARM MTE 标签插入: 已添加 AArch64 指令支持 (IRG/ADDG/SUBG/STG/LDG/SETTCO 指令编码已实现于 aarch64.rs)，但尚未集成到 sanitizer 流程。完整实现需要: 函数 prologue 分配 tag 栈帧、指针标记逻辑、软件 tag 模式检查 |
| KASAN-kernel (`-fsanitize=kernel-address`) | ✅ 内联 shadow 地址计算 | x86_64: shadow = (addr >> 3) + 0xdffffc0000000000; 不依赖运行时函数 |
| UBSAN (`-fsanitize=undefined`) | TU init + `Div/Mod` 前 `__ubsan_handle_divrem_overflow(0, divisor)`；`Shl/Shr` 前 `__ubsan_handle_shift_out_of_bounds(0, shift_amt)`；有符号 `Add/Sub/Mul` 前 `__ubsan_handle_signed_integer_overflow(0, lhs)`；VLA bound 前 `__ubsan_handle_vla_bound_not_positive(bound)`；`Deref` 前 `__ubsan_handle_null_pointer_dereference`；float→int 前 `__ubsan_handle_float_cast_overflow` | — |
| KCSAN (`-fsanitize=thread` / `kernel-kcsan`) | TU init + 每函数 `__kcsan_func_entry`；内存访问 watchpoint 与 KASAN 共享 load/store 通道 | 专用 `__kcsan_check_access` 路径未独立发出 |
| KCOV (`-fsanitize-coverage`) | 每函数 `__sanitizer_cov_trace_pc` + `__dcc_cov_guards_N[16]` 计数器 + `__sanitizer_cov_trace_pc_guard` 在 if / while / do / for / case / label / goto 等边 | ✅ 跨函数 inline 合并已实现 (`profile::kcov_merge`): 调用图分析、全局 guard 索引、`__sanitizer_cov_weak_hook_*` 生成 |
| Fuzzer (`-fsanitize=fuzzer`) | KCOV 全部 + 比较 `__sanitizer_cov_trace_cmp1/2/4/8` + switch dispatch `__sanitizer_cov_trace_switch` | libFuzzer 协议未实现。需实现: `LLVMFuzzerTestOneInput` 入口包装、8-bit counters 初始化、PC table (`__start___pc_table`/`__stop___pc_table`) 生成、malloc/free 拦截器 (`__sanitizer_weak_hook_malloc`/`__sanitizer_weak_hook_free`) |

实现位置：`src/sanitizer.rs::instrument_program()`，在 driver parse 之后、
IR lower 之前由 AST walker 注入。CLI 由 `SanitizerFlags::from_args` 解析。
`no_sanitize` 属性按通道关闭；KCOV / Fuzzer 不受 `no_sanitize` 影响。
`.dcc.san_info` 节 v2，载荷为 8 字节 magic + 版本 + 6 标志字节。

---

## 4. C 前端 parser / IR（`src/parser.rs` + `src/ir.rs`）

### 4.1 解析层（已基本完成）

✅ 见 `docs/c_standard.md`：C89 / C99 / C11 / C17 / C23 + GNU 扩展全部覆盖；
MS 扩展 + `#pragma` 全家族 + `_Pragma` 静默接受（M11.1）；
`__attribute__((cleanup/no_sanitize/target_clones/copy/returns_nonnull))` 全部
（M11.1）；`_Static_assert` 字符串化 + C23 单参形式（M11.1）；
`__builtin_offsetof/huge_val*/nan*/isnan/isinf/finite/signbit/fpclassify`
（M11.2）；`__builtin_call_with_static_chain` / 嵌套函数 / computed goto /
asm goto 输出约束（M11.2）；内联 asm 约束全家族（M11.3）。

🟠 已识别但简化（多数已交付，详见 M14）：

| 特性 | 现状 (M14) |
|------|------|
| 预置 C11 `atomic_*` 宏 | hosted 模式预置；freestanding 模式显式 `__STDC_NO_ATOMICS__=1` + `__STDC_HOSTED__=0` 且不预置（与内核冲突） |
| `__attribute__((returns_nonnull))` | 解析 + codegen：返回前 `test rax, rax; jnz .ok; ud2; .ok:` 强制非空 |
| `__attribute__((target_clones(...)))` | 解析并存为 `FuncDecl.target_clones`；IFUNC resolver 由 `link/target_clones.rs` 生成 STT_GNU_IFUNC + R_X86_64_IRELATIVE |
| `__attribute__((weak(...)))` 与 `#pragma weak name` | 解析并由 codegen 写 `STB_WEAK` 绑定；`#pragma weak` 走 `ParseOptions.weak_symbols` 通路 |
| K&R 函数 | 解析自动检测 + 标 `is_old_style`；codegen 走与 ANSI 相同路径 |
| 嵌套块重声明 | `check_var_redecl` 允许函数体内遮蔽；文件作用域类型不匹配仍 E0007 |
| MS 扩展关键字 | `__declspec(thread)` → TLS；`__declspec(noreturn)` → cold+noinline；`__declspec(align(N))` → 对齐；dllimport/dllexport 接受无 codegen 差异 |
| `__pragma` | 接受（无操作） |

### 4.2 编译期常量求值（`src/const_eval.rs`）

🟢 M15 已实装（`eval_full` 返回 `ConstValue` 枚举；旧 `eval` 仍仅取整数部分）：

- 浮点常量求值（`Float(f64)`）：`+ - * /`、混合 `Int<->Float`、`==/!=/</...` 比较提升到 f64
- 字符串字面量作为常量（`ConstValue::Str { data, label }`，FNV-1a 64 标签）
- 地址常量（`&ident` / `&arr[i]` / `&&label`）→ `ConstValue::Addr { name, offset }`
- `__builtin_*` 编译期折叠（新增：`__builtin_strlen`/`__builtin_abs/labs/llabs`/
  `__builtin_bswap16/32/64`/`__builtin_expect`/`__builtin_popcount*`）
- 字符串字面量拼接在 parser 层完成（`while TokKind::StringLit { data.extend(...) }`），
  const_eval 看到的就是合并后的单个 `Expr::String`
- C23 constexpr 函数：仅作为函数声明解析（`while "constexpr" { advance }`），
  无强制编译期求值（与 `inline`/`__attribute__((always_inline))` 等价处理）
- `#embed` 数据字节数：解析为字节数组，sizeof / `__builtin_strlen` 可计算其长度
- 严格类型检查 warning（`-Wint-truncation` / `-Wint-conversion`）：cast 阶段检测
  隐式指针↔整数转换 + 整数截断，不阻塞编译（按 warning 类别报告）

🟢 M15 运行时检查：

- `-ftrapv` 有符号算术溢出 trap：binop 后置 `jo` 跳到 `__dcc_trapv_overflow`
  桩函数（用户需链接 runtime helper）；覆盖 `Add/Sub/Mul/Div`
- `-fbounds-check` 数组下标运行时边界检查：仅对 `ArraySize::Const(n)` 的数组
  插入 `cmp rcx, n*es; jb ok; call __ubsan_handle_out_of_bounds` 序列

🟠 已知预存限制：

### 4.3 Typed IR 验证（`src/ir.rs`）

🟠 已识别 E10xx 但部分能力为 noop：

| 码 | 含义 | 当前能力 |
|----|------|----------|
| E1001 | 未解析类型 | ✅ |
| E1002 | 函数无名 | ✅ |
| E1003 | float 不支持运算 | ✅ 拒绝 + noop emit |
| E1004 | 非法解引用 | ✅ |
| E1005 | va_arg operand | ✅ |
| E1006 | 全局 init 非常量 | ✅ 拒绝；支持整数/浮点/字符串/复合字面量 |
| E1007 | 向量运算不支持 | ✅ 拒绝；只允许 +/-/* |
| E1008 | 复数运算不支持 | ✅ 拒绝 |
| E1009 | 未知成员 | ✅ |
| E1010 | 非聚合成员访问 | ✅ |

🟢 M15 已实装：

- 严格类型检查：`-Wint-truncation` 检测 `(char)0x100` 类截断；
  `-Wint-conversion` 检测隐式指针↔整数转换；仅 warning，不阻塞
- 数组越界检测：`-fbounds-check` 对 `ArraySize::Const(n)` 数组插入
  `cmp rcx, n*es; jb ok; call __ubsan_handle_out_of_bounds`
- 整数溢出 trap：`-ftrapv` 在 `Add/Sub/Mul/Div` 后置 `jo` 跳到
  `__dcc_trapv_overflow` 桩函数（runtime helper 由用户链接）

---

## 5. 优化 pass（`src/opt.rs` + `src/ssa_gvn.rs` + `src/loop_opt.rs`）

✅ `-O0` always_inline / `-O1` 常量折叠 + 内联 / `-O2` GVN/CSE/LICM/分支折叠 /
`-O3` 循环展开（小 trip≤4）+ 向量元素循环向量化（标量后端）。

### M16 新增（`src/ssa.rs` + `src/ssa_passes.rs`）

引入轻量 SSA IR（BB / CFG / phi / dominator）作为新 pass 的工作面，
AST ↔ SSA 双向桥由 `ssa::build_from_func` / `ssa::rebuild_block` 提供，
结构化函数（if/while/do/for/break/continue）跑完整 SSA，含 goto/
label/switch 的函数自动降级回原 AST 路径。

| Pass | 状态 (M16 后) |
|------|---------------|
| 死代码消除 (DCE) 跨函数 / 跨 TU | ✅ `cross_function_dce`：从 `main` + extern 做 call graph BFS，删除不可达 internal static 函数（`opt::optimize` 在 -O2+ 调用） |
| 强度削减 (Strength Reduction) | ✅ `strength_reduction`：`i * 2^N` → `i << N`，`i / 2^N` → `i >> N` |
| 归纳变量优化 (IVOP) | ✅ `ivop`：识别 induction × constant 模式，标记给 strength_reduction 接续 |
| 别名分析 (Alias Analysis) | ✅ `alias_analysis`：Steensgaard 风格 union-find（field-insensitive），返回 SSA 名 → set id 映射 |
| 调度 (Instruction Scheduling) | ✅ `instruction_scheduling`：BB 内 list scheduling，pure 运算集中到 BB 顶端（-O3 启用） |
| Profile-Guided Optimization (PGO) | ✅ `pgo` / `load_profile`：读 `<func>.dcc.profdata`（text 格式 `bb <name> <count>`），hot/cold BB 插入 `__dcc_*_hint` Store（-O3 启用）。instrumentation 路径待补 |
| Auto-parallelization (OpenMP-like) | ✅ `auto_parallelization`：识别安全 `for.body.*` BB，emit `__dcc_parallel_hint`（-O3 启用）。SIMD 代码生成待后端消费 |
| 循环分块 (Loop Tiling) | ✅ `loop_tiling`：检测小循环体（≤4 inst），unroll-and-jam 简化版 |
| 循环交换 (Loop Interchange) | ✅ `loop_interchange`：检测 `*.i.*` + `*.j.*` 嵌套，entry 插入 `__dcc_loop_interchanged` hint |
| 尾部调用优化 (TCO) | ✅ `tail_call_optimization`：检测 exit BB 含对自身的 `Call`，提示 codegen 改 jump |
| 函数属性传播 (noreturn/cold/hot) | ✅ `function_attribute_propagation`：`is_cold` 函数 entry 加 `__dcc_section_cold` Store 提示；`is_hot` / `is_noreturn` 由 SSA 字段透传 |
| 寄存器分配 (Register Allocation) | ✅ `register_allocation`：线性扫描占位（hint 模式）。真实物理寄存器仍由 codegen 的硬编码 GP/XMM 池负责 |
| 别名集合 (Alias Set) | ✅ `alias_set`：基于 P4 set 划分，三档判别（identity / disjoint / overlap） |
| Speculative execution | ✅ `speculative_execution`：BB 内 pure 指令集中到 BB 顶端，便于分支条件尽早算出 |
| 常量传播 (Sparse Conditional) | ✅ `sparse_conditional_constant_propagation`：经典 lattice 分析（Undef/Const/Top），对纯运算做常量替换 + 删除冗余指令 |

**测试**：`tests/fixtures/opt/*.c` 7 个（ssa_ir / strength_reduction /
cross_function_dce / sccp / loop_opt / tco / function_attr），
`tests/integration.rs` 对应 `m16_*` 函数在本地 `cargo test m16_`
验证。详见 `docs/architecture.md` M16 章节。

---

## 6. C++ 前端（`src/cpp/`）

### 6.1 已实装（M7-M10+）

✅ 命名空间 / 运算符 / 继承 (单/多/钻石) / 虚函数 / vtable (`_ZTV<Name>`) /
typeinfo (`_ZTI<Name>`) / RTTI / 异常 (Itanium zero-cost) / lambda / 模板完整单态化
(src/cpp/template.rs) / ctor 初始化列表 / 隐式 this 注入 / 局部类变量 /
`co_await`/`co_yield`/`co_return` 协程 (src/cpp/coroutine.rs) /
module BMF 序列化 (src/cpp/bmf.rs) / GCC 兼容运行时 (src/cpp/gcc_compat.rs) /
C++20 占位 (module/import/export/concept/requires) /
M10+ SFINAE / partial spec / 完整 concept 求值 / operator-> 链 /
跨虚继承 dynamic_cast / 构造委托 (`using Base::Base;`) / 成员函数指针 /
`__cxa_guard_*` 线程安全 init guard / `std::initializer_list`。

🟠 简化 / placeholder：

| 模块 | 现状 |
|------|------|
| 模板实例化 | 🟢 (M25) `src/cpp/template.rs` 完整单态化：`TemplateRegistry` 注册 class/function 模板及偏特化 (含 requires-clause 约束)；`instantiate` 按需实例化 + 缓存；`pick_best_specialization` 按 match score 选择最佳偏特化；解析模板-id 调用表达式 `T<U>`、模板局部变量声明、typeid 模板类型；每个实例化定义只 emit 一次 |
| `obj.*ptr_to_member` (M8 简化) | 🟢 (M25) MemberStar / PtrMemberStar 路径实装, 解析 `.*` 与 `->*` 语法, 走 `__dcc_mbr_<class>_<member>` 偏移 |
| `typeid(e)` (M8 简化) | 🟢 (M25) `CppExpr::Typeid` 和 `TypeidType` 走真 RTTI: `expr_static_class` 推断静态类, 生成 `(&__dcc_ti_<class>)` 符号引用, 替代 M8 stub `((_ZTI*)(((void*)e)->__vptr + 0))`; 跨多态指针动态 RTTI 需 vtable 中写 typeinfo 指针 (当前 emit_vtable RTTI 槽为占位 0, lower.rs#L1743)。需实现: 在 `emit_vtable` 中生成 `(&__dcc_ti_<class>)` 而非 `(void*)0`, 并确保 typeinfo 符号在链接时可见 |
| `auto` (M17 实装) | `infer_type` 真类型推导: int 字面量→Int, `"abc"`→`const char*` (`MangleType::Pointer(Int)`), 类名已知 → `MangleType::Class` |
| `auto x = expr;` | M17 真推导: `Widget w(7)` 拆为 `Widget w; _ZN6WidgetC1Ei(&w, 7)` |
| `decltype(expr)` (M17 实装) | 解析为 `MangleType::Int` 占位 (M17.0 简化) |
| `decltype(auto)` (M17 实装) | 解析为 `MangleType::DecltypeAuto` 占位 |
| `using Base::Base;` 继承链 ctor (M17 实装) | 类内 `using` 被 lower 接受, InheritingCtor 节点继续复用既有 lower; 多文件链接场景受 dcc C 后端嵌套字段访问限制 (`__base_Base1` 字段生成方式与直接字段不同, lower.rs#L1593)。需实现: 在 C 后端添加嵌套字段访问支持, 或修改 lower 生成扁平字段而非嵌套 struct |
| `co_yield` / `co_await` / `co_return` (M10+) | 🟢 (M25) `src/cpp/coroutine.rs`：Duff's device 状态机，per-function 变量 `<fn>_coro_state`/`<fn>_coro_value`/`<fn>_coro_done`；`emit_fn` 包裹协程体为 switch 结构，case 0 入口 + resume labels 在每 suspend 点 |
| `concept` / `requires` (M10+) | 解析为注释 + 简化求值；不与 SFINAE 完整联动 |
| `module` / `import` (M10+) | 🟢 (M25) `src/cpp/bmf.rs`：DCC_BMF_V1 文本格式，lower 在 export module 声明后收集 exports；`ModuleInfo::to_bmf`/`from_bmf` round-trip；ODR 检查待后续 |
| C++ 标准库 (`src/cpp/stdlib/`) | 头文件 + 运行时为 C 实现；`vector<int>` 等模板实参语法无法直接使用 |
| 模板基类 (M8) | 不支持 |
| 模板友元 | 不支持 |
| 模板默认实参 | 解析不强求 |
| 模板偏特化的多参数 | 🟢 (M23) 多形参 pattern 匹配实装 |
| `export template` | 注释占位 |
| 协程 promise type | 🟢 (M23) `CoroutinePromiseType` 推导 + C 兼容降级定义 |
| `noexcept` (M17 实装) | 解析 + lower 端 emit `__attribute__((nothrow))` 函数属性 |
| `final` / `override` (M17 实装) | 解析 + 接受; vtable/devirt 端在 v0 简化, 派生尝试覆盖会 mangled 冲突 |
| `alignas` / `alignof` | 解析为 C11 `alignas` 走标准路径 |
| `if constexpr` (M17 实装) | 编译期分支: 通过 const_eval 在 lower 端选分支 |
| `explicit` 转换构造函数 (M17 实装) | 解析层接受, 仍可显式调用 (隐式转换检查在 v0 简化) |
| `mutable` 成员 (M17 实装) | 解析层接受, 独立字段 (const 成员函数中可写) |
| `volatile` 成员函数 (M17 实装) | 解析层接受, lower 端标记 (C 端 volatile 与 C++ volatile 在 ABI 上等价) |
| `friend` (M17 实装) | 解析层接受, lower 端走 FreeFn 路径 (无 this 形参, 无类作用域 mangle) |
| `std::filesystem` (M18 实装) | path / file_type / exists / is_directory / create_directory / rename / remove / file_size / directory_iterator (包 opendir/readdir); 头在 `src/cpp/stdlib/headers/filesystem` |
| `std::thread` / `std::mutex` / `std::recursive_mutex` (M18 实装) | pthread 包装; lock/try_lock/unlock + this_thread::sleep_for/yield; 头在 `thread`/`mutex` |
| `std::condition_variable` (M18 实装) | pthread_cond_t 包装; wait/wait_for/notify_one/notify_all; 头在 `condition_variable` |
| `std::chrono` (M18 实装) | system_clock/steady_clock/high_resolution_clock + duration/time_point; 内部 int64 纳秒 + clock_gettime; 头在 `chrono` |
| `std::expected` (M18 实装, C++23) | expected_int/expected_str 两种擦除特化; 通用 dccpp_expected_make_val/err/destroy 走 void*+析构回调; 头在 `expected` |
| `std::flat_map` (M18 实装, C++23) | flat_map_ii (int→int) 排序数组 + 二分查找; insert/get/contains/erase/clear; 头在 `flat_map` |
| C++23 多维下标 `obj[i,j,k]` (M18 实装) | parser 构造 `CppExpr::MultiIndex`; lower 改写为 `__dcc_multi_index_N(obj, i, ..., N)`; 运行时提供 N=2/3/4 占位 (返回 0) |
| `std::regex` (M18 实装, 简化子集) | 编译 regex 为 byte-code; 解释器用递归回溯 NFA; 支持字面/`.`/`[abc]`/`[a-z]`/`[^...]`/`*`/`+`/`?`/`|`/`()`/`^`/`$`/`\x`; 头在 `regex` |
| `std::ranges` / `std::views` (M18 实装, 简化) | filter/transform/take/reverse view (eager materialize); 头在 `ranges` |
| Concepts (M18 实装, SFINAE 简化版) | `concept Name = ...;` 解析 + 接受; 不做实际 SFINAE 过滤, 模板约束走 free path |
| C++20 Modules (M18 占位) | 🟢 (M25) BMF 序列化已实装 (src/cpp/bmf.rs)；ODR 检查待后续 |
| `static` 类成员 | 🟢 (M23) 解析为 C `static` 字段 + Itanium ABI mangling (`_ZN<class><name>E`) |
| `virtual` 继承 | 部分支持（见 `M10+`），复杂虚基类不验证 |
| `placement new` | 🟢 (M25) parser 检测 `new (ptr) T(args)` 形式构造 `CppExpr::PlacementNew { ptr, ty, args }`, lower emit `((void*)ptr, ctor(ptr, args))` 走原地构造, 不调用 operator new 分配新内存 |
| `operator new[]` / `operator delete[]` | 🟢 (M25) lower 走 mangle_op_new_array() / mangle_op_delete_array() 命名常量, 即 Itanium C++ ABI `_Znam` / `_ZdaPv` 数组分配/释放符号; 符号由 dcc 运行时 dccpp_runtime.c 声明 (extern) 并最终转发到 libstdc++ / libc malloc |
| `new T[n]` 数组分配 | 🟢 (M25) parser 识别 `new T[n]` 形式构造 `CppExpr::NewArr { ty, count }`, lower 走 mangle_op_new_array() 调用, 即 Itanium C++ ABI `_Znam` 数组分配; 配合 `delete[] p` → `_ZdaPv` 完整 array 生命周期 |

🔴 完全没有：

- (M18 全部实装) `std::filesystem` / `std::thread` / `std::mutex` /
  `std::condition_variable` / `std::chrono` / `std::regex` /
  C++23 多维下标 / `std::expected` / `std::flat_map` /
  `std::ranges` / `std::views` / Concepts SFINAE 简化版 /
  C++20 Modules 占位 — 全部见第 6 节表格 M18 行

---

## 7. 预处理器（`src/preproc.rs`）

🟢 / 🟠 / 🔴 状态（2026-07-20 M19 之后）：

| 特性 | 现状 |
|------|------|
| `__has_include` / `__has_attribute` / `__has_builtin` | ✅ |
| `#include_next` | ✅ |
| `#warning` / `#error` | ✅ |
| `#pragma` 全家族 + `_Pragma` | ✅ (M11.1) |
| `#embed` (C23) | ✅ 字节数组嵌入 |
| `#elifdef` / `#elifndef` (C23) | ✅ |
| 数字分隔符 `1'000'000` | ✅ |
| 宏展开涂蓝继承 / 防栈溢出 | ✅（缓存 V7） |
| 宏 `#` / `##` 字符串化 | ✅ |
| `__VA_OPT__` (C++20 / C23) | ✅ (M19) 完整支持: 空参整段 (含前导空白) 移除, 非空展开 content, 嵌套按内→外顺序消解 |
| `__FILE__` / `__LINE__` / `__DATE__` / `__TIME__` | ✅ |
| `__COUNTER__` | ✅ (M19) 单调递增, 跨宏嵌套共享 preproc 状态; 在 `#if` 中可被求值 |
| `__pragma` (MS) | ✅ 静默接受 |
| 模块化 include guards (C23 `#pragma once`) | ✅ |
| 条件 token 拼接 | ✅ (M19 解决: 缓存 hash 现在把 -D 定义纳入, _FORTIFY_SOURCE 等变值自动失效) |
| 宏递归展开防护 | ✅（V7 include cache） |
| 跨 TU `assert.h` 行为 | ✅ (M19) 缓存键包含全部宏表 (按名排序的 name+body+params+variadic XOR), `_FORTIFY_SOURCE` 等变值在 `-D` 切换时自动失配 |
| 字符集 / locale (C23 `#pragma locale`) | 🟢 (M23) 支持 `default`/`reset`/`"POSIX"`/`"UTF-8"`/`"ISO-8859-1"` 字符集切换, charset 字段记录当前编码 |
| 预定义宏（如 `__linux__`, `__GNUC__`, `__amd64__`） | 🟢 (M19) 补齐: `__amd64` / `__amd64__` / `__x86_64` / `__k8` / `__k8__` / `__USER_LABEL_PREFIX__` / `__VERSION__` / `__OPTIMIZE__` / `__OPTIMIZE_SIZE__` / `__NO_INLINE__` / `__FINITE_MATH_ONLY__` / `__PIC__` / `__PIE__` / `__code_model_small__` / `__SEG_GS` / `__SEG_FS` |
| 依赖文件 -MD / -MMD / -MP | ✅（但与 host GCC 略有差异） |
| 增量预处理（pch） | 🟢 (M25) `src/preproc.rs::PchOptions` / `PchBundle` / `serialize` / `apply` 走完整的"创建 + 加载 + 宏状态哈希校验"路径; CLI `-fpch-include=path` / `-fpch-create=path` / `-fpch-include-only=path` 在 `src/cli.rs` 已接受; 7 个单元测试 + 1 个集成测试 `pch_create_then_include_compiles_and_runs` 覆盖 create→include 端到端 |

> **M26 增量（2026-08-06）— C23 预处理全家族测试覆盖补全**：
> 实现此前已全部交付 (本节各 ✅ 行), 本次为无测试覆盖的 C23 特性补齐
> 回归: 新增 27 个单元测试 + 10 个集成测试 (含 fixture
> `tests/fixtures/c/elifdef.c` / `tests/fixtures/c/has_macros.c`):
> - `#elifdef` / `#elifndef`: 已/未定义宏分支选择、`#else` 后报 E0200、
>   与 `#if`/`#elif`/`#else` 嵌套组合 (`elifdef_*` / `elifndef_*` /
>   `if_elif_else_nested_branches` + `m26_elifdef_elifndef_runs`)
> - `#embed`: 字节嵌入、`limit(N)` 截断、`prefix`/`suffix` 包裹、
>   空文件 `if_empty` 替代、`#embed` 须独占一行 (行内形式不被识别)
>   (`embed_*` 单测 + `m26_embed_limit_prefix_suffix_runs` /
>   `m26_embed_if_empty_runs`)
> - `__has_embed`: 文件存在/缺失求值、带 `limit` 参数形式
>   (`has_embed_*` + `m26_has_embed_selects_branch`)
> - `__has_c_attribute`: 已知 (fallthrough/deprecated) → 1, 未知 → 0
> - `__has_include`: 存在/缺失 (含 -I 搜索路径)
> - `__has_builtin` / `__has_attribute`: 已知/未知求值
> - `__STDC_ENDIAN_*`: 仅 C23 模式预定义, NATIVE==LITTLE (小端主机),
>   非 C23 模式 defined 为假
> - `#pragma once`: 重复 include 去重 (include 注释唯一, 宏只定义一次)
> - `#warning` / `#error`: 活动分支发 W0200/E0203, 非活动分支静默
> - `#include_next`: 多搜索路径链式解析 (单测 + `m26_include_next_chain`)
> - 预定义宏 (`__linux__`/`__x86_64__`/`__GNUC__`) 在 `#if` 中可求值
> 总计: 539 unit + 284 integration 全过, clippy 0 警告。

M19 关键改动:

- `__COUNTER__` 在 `Preprocessor` 中以 `u32` 状态字段持有, `expand_with_disabled_inner` 每次访问自增 (wrapping).
- `__VA_OPT__` 在 `substitute_params` 入口按"是否可变参数宏 + __VA_ARGS__ 是否非空"二分支消解, 解析时跳过字符串/字符字面量, 处理前后配对的 `(` 与嵌套.
- 预定义宏全部移到 `Preprocessor::new` 旁的 `define_builtin` 调用点, 不依赖外部 libc 头.
- include 缓存版本从 V6 → V7, 任何 `hash_macro_state` 变化都让缓存自动失配.
- `expand_with_disabled_inner` 在函数宏调用前增加 `raw_args.is_empty() && !params.is_empty()` 分支: 当宏期待 ≥1 实参却被空括号 (`FOO()`) 调用时, 按 GCC 行为视为 N 个空实参展开. 修复 glibc 头 `__STRING ()` / `__ASMNAME2` 链在 dcc 预处理器里残留 `__STRING` 字面量的问题, 使 `<stdio.h>` / `<stdlib.h>` 在 dcc 下能被正常解析.

---

## 8. 调试 / 诊断 / 性能工具

### 8.1 调试信息（`src/debug.rs` + `src/codegen/elf.rs`）

🟢 / 🟠 状态（2026-07-24 M24 之后）：

- DWARF v4：✅ x86_64 / ✅ AArch64 (M8) / ✅ RISC-V (M8)
- `.BTF`：✅ x86_64（struct dcc_probe）/ ✅ AArch64 (M8) / ✅ RISC-V (M8)
- `.eh_frame`：✅ x86_64（CIE + FDE）/ ✅ AArch64 (M8) / ✅ RISC-V (M8)
- `.orc_unwind_ip`：✅ x86_64 / ✅ AArch64 (M8) / ✅ RISC-V (M8)
- DWARF line table：🟢 M24 增强：相邻同源同行条目在 `-g1` 下自动去重，
  减少 GDB 行号表体积；matrix optimization special opcode (pc 推进 <
  min_insn) 仍走 1 字节优化；DW_LNS_const_add_pc 增强路径经分析暂不启用
  （line_advance 冲突的固有 trade-off，代码中含详细注释）。
- DWARF type info：🟢 M24 增强：`DwarfMember` 加 `bit_size` 字段与
  `regular()` / `bitfield()` 构造函数；位域成员正确编码
  `DW_AT_bit_size` (0x0d) + `DW_AT_bit_offset` (0x0e) 属性，非位域成员
  只编码 `DW_AT_data_member_location`。
- DWARF call frame：✅ CIE + FDE
- `-gsplit-dwarf` / `-gpubnames` | 🟢 M23 已实装（split-dwarf 支持 `.dwo` 文件生成）
- GDB index (`-Wl,--gdb-index`) | 🟢 M22 已实装（最小 `.gdb_index` 节占位）
- AArch64/RISC-V DWARF / BTF | ✅ M8 已实装（`build_dwarf_full` + `.BTF` 节）

> **M24 增量（2026-07-24）**：
> - `DwarfMember` 新增 `bit_size: u32` 字段；提供
>   `DwarfMember::regular(name, offset_bits, ty)` 与
>   `DwarfMember::bitfield(name, offset_bits, bit_size, ty)` 两个
>   明确语义的构造器。
> - `write_aggregate_die()` 在 `m.bit_size > 0` 时使用位域属性集
>   （`DW_AT_bit_size` + `DW_AT_bit_offset` 均为 `DW_FORM_data1`），
>   写入 `bit_size` 与位内偏移 `offset_bits % 8`。
> - `build_line_program()` 在 `opts.level <= 1` 模式下对相邻
>   `file_idx / line / column / is_stmt / discriminator` 五元组相同
>   的 line entry 去重，仅保留第一条（不影响 pc 推进，最后一条的
>   end_sequence 仍正常发射）。`level >= 2` 不去重，保留全部条目。
> - 新增 4 个单测验证：
>   - `dwarf_type_bitfield_emits_bit_size_and_offset`
>   - `dwarf_regular_member_has_no_bit_size`
>   - `dwarf_line_dedup_under_g1`
>   - `dwarf_line_no_dedup_under_g3`
> - 总测试 389 unit + 175 integration 全过。
>
> **M24 增量（2026-07-24）— Section 11.5 setjmp CFI 寄存器保存表**：
> - `src/debug.rs` `FuncFrame` 增加 `cfi_ops: Vec<u8>` 字段并提供
>   `FuncFrame::new()` / `FuncFrame::with_cfi()` 构造器；
>   `build_eh_frame_with_arch` 在 FDE body 内嵌入 `func.cfi_ops`。
> - `src/codegen/x86_64.rs` 新增 `build_cfi_ops()` 构造
>   `DW_CFA_def_cfa(7, 16|24)` + `DW_CFA_offset(13, 2)` +
>   `DW_CFA_offset(16, 1)` (+ `DW_CFA_offset(6, 3)` 当
>   `-fno-omit-frame-pointer`)，CFA 偏移与 dcc prologue
>   (push r13 [+ push rbp]) 一一对应；新增 `func_frames()`
>   供 `src/codegen/mod.rs` 统一调用。
> - 修复 3 处 `.eh_frame` CIE 预存 bug：
>   - `CIE_id`: 0xFFFFFFFF (DWARF `.debug_frame` 约定) → 0 (LSB)
>   - `CIE_pointer` 公式符号反了 → `(fde_start+4) - cie_len_pos`
>   - CIE 对齐从 `eh.len() % 8 != 4` 改为 CIE 相对的 4 字节对齐，
>     避免插入 6 字节多余 CFA_nop
> - 迁移 `aarch64` / `arm` / `mips` / `riscv64` 四后端的
>   `func_frames()` 到 `FuncFrame::new()`，等待 CFI ops 后续实装。
> - 新增 4 个单测/集成测试验证：
>   - `eh_frame_fde_emits_cfi_ops_for_setjmp` (debug::tests)
>   - `eh_frame_fde_omits_cfi_ops_when_empty` (debug::tests)
>   - `setjmp_x86_64_fixture_compiles` (integration)
>   - `setjmp_x86_64_eh_frame_has_cfi_ops` (integration)
> - `dcc .o` + 主机 gcc 链接验证：setjmp/longjmp 运行时返回 49
>   (与手写 .c 期望一致)。

> **M25 增量（2026-07-24）— Section 11 P0.1 wchar_t libc 转发**：
> - `src/link/resolve.rs` `is_implicit_undef` 白名单补齐 11 个
>   wcs*/wmem* libc 入口: `wcscpy` / `wcscat` / `wcscmp` /
>   `wcsncmp` / `wcslen` / `wcschr` / `wcsrchr` / `wmemcpy` /
>   `wmemmove` / `wmemset` / `wmemcmp` (原本走 freestanding linker
>   会报 undefined reference, 允许为 implicit undefined 以让
>   host gcc 在 -lc 下解析)。
> - 新增 `tests/fixtures/c/wchar.c` 覆盖 wcslen / wcschr /
>   wcsrchr / wmemcpy / wmemcmp / wmemset / wmemmove / wcscmp /
>   wcsncmp / wcscat 运行时行为。
> - 新增 integration test `wchar_libc_fixture_runs` 与 helper
>   `run_host_link_and_run` (dcc -c + host gcc link + run).
> - Section 11 表格 `wchar_t` 行 🟠 → 🟢 (M25)。

> **M25 增量（2026-07-24）— Section 11 P0.2 signal libc 转发**：
> - `src/link/resolve.rs` `is_implicit_undef` 白名单补齐
>   `sigaction` / `sigaltstack` / `signal` 三个 POSIX 信号入口。
> - 新增 `tests/fixtures/c/signal.c` + 集成测试
>   `signal_libc_fixture_runs` 验证 `signal(SIGTERM, handler)`
>   运行时返回非 NULL 旧 handler (即 prev != handler 0)。
> - 新增集成测试 `sigaction_sigaltstack_fixture_compiles`
>   验证 dcc 编译带 `struct sigaction` / `stack_t` 参数的代码,
>   且生成的 .o 引用 `sigaction` / `sigaltstack` 作为 undefined
>   symbol (host gcc 链接时 -lc 解析).
> - 运行时验证：sigaction() / sigaltstack() / signal() 运行时
>   调用成功，struct sigaction / stack_t 布局与 glibc 一致。
> - Section 11 表格 `信号` 行 🔴 → 🟢 (M25)。

> **M25 增量（2026-07-24）— Section 11 P0.3 复杂数学 libm 转发**：
> - `src/link/resolve.rs` `is_implicit_undef` 白名单补齐
>   `cproj` / `cabs` / `carg` 三个复杂数学入口 (走 libm).
> - 新增 `tests/fixtures/c/complex_libm.c` + 集成测试
>   `complex_libm_fixture_runs` 用 dcc -c + host gcc -lm 链接
>   运行, 验证三个函数能成功调用并返回 0 退出码.
> - 运行时限制: dcc 已实现 `_Complex` 字面量、`__real__` /
>   `__imag__` 关键字、16 字节 struct 传值 ABI (含 _Complex
>   返回路径修复) 及 _Complex 算术运算 (加/减/乘/除)。
> - Section 11 表格 `复杂数学函数` 行 🟠 → 🟢 (M25).

> **M25 增量（2026-07-24）— Section 4.2 P1.1 16 字节/聚合 KASAN**：
> - `src/sanitizer.rs::kasan_load_fn` / `kasan_store_fn` 对 N > 16
>   走 `__asan_loadN` / `__asan_storeN` 通用名 (N 即实际 size),
>   而非之前的通用 `__asan_load` / `__asan_store`.
> - `make_mem_check_call` 对 size > 16 自动 emit 双参形式
>   `__asan_loadN(addr, size_t)`, 16 字节及以下仍 emit 单参
>   `__asan_loadN(addr)`. 与 LLVM asan_interface.h 的 N > 16
>   双参 variant 对齐.
> - 新增 6 个单元测试: `kasan_load_fn_maps_known_sizes` /
>   `kasan_load_fn_emits_generic_for_aggregate_sizes` /
>   `kasan_store_fn_maps_known_sizes` /
>   `kasan_store_fn_emits_generic_for_aggregate_sizes` /
>   `make_mem_check_call_emits_size_arg_for_aggregate_kasan` /
>   `make_mem_check_call_emits_single_arg_for_small_kasan`.
>   24 个 sanitizer 单元测试 0 失败.
> - Section 4.2 已知预存限制列表移除 `16 字节/聚合 KASAN load/
>   store 未单独 emit` 一条 (M25).
> - 运行时限制: 用户需自行提供匹配的 `__asan_load24` /
>   `__asan_load32` 等符号 (LLVM 编译器-rt 包含). dcc 仅
>   emit 调用.

> **M25 增量（2026-07-24）— Section 4.2 P1.2 VLA bound UBSAN**：
> - `src/sanitizer.rs::vla_bound_check` 已在 M15 阶段实装
>   (`__ubsan_handle_vla_bound_not_positive(bound)`) 且带
>   单元测试 `ubsan_vla_bound_check_inserted`. P1.2 PR
>   仅补齐集成层覆盖.
> - 新增集成测试 `ubsan_vla_bound_check_emitted` (在
>   `tests/integration.rs`): 编译带 `int vla[n]` 的 C
>   代码 + `-fsanitize=undefined`, 验证汇编层含
>   `call __ubsan_handle_vla_bound_not_positive`. 用
>   `-S` 检查汇编而非 .o 的 strtab 是因为 dcc 当前对
>   未声明的外部 sanitizer 函数不会自动写入
>   `.symtab`/`.strtab` (Section 9 follow-up), 汇编层
>   已正确 emit 调用。
> - Section 4.2 已知预存限制列表移除 `VLA UBSAN trap
>   变体未实装` 一条 (M25).

> **M25 增量（2026-07-24）— Section 4.2 P1.3+P1.4 null deref 与
> float-cast UBSAN 集成覆盖**：
> - `src/sanitizer.rs::null_deref_check` 与
>   `src/sanitizer.rs::float_cast_check` 已在 M15 阶段实装
>   (`__ubsan_handle_null_pointer_dereference` 与
>   `__ubsan_handle_float_cast_overflow`) 且带单元测试
>   `ubsan_null_deref_check_inserted` /
>   `ubsan_float_cast_check_inserted`. P1.3+P1.4 PR
>   仅补齐集成层覆盖。
> - 新增集成测试 `ubsan_null_deref_check_emitted` 与
>   `ubsan_float_cast_check_emitted` (在 `tests/integration.rs`):
>   编译带指针解引用 / float→int cast 的 C 代码 +
>   `-fsanitize=undefined -S`, 验证汇编层含
>   `call __ubsan_handle_null_pointer_dereference` /
>   `call __ubsan_handle_float_cast_overflow`. 用 `-S`
>   检查汇编而非 .o 的 strtab 是因为 dcc 当前对未声明的
>   外部 sanitizer 函数不会自动写入 `.symtab`/`.strtab`
>   (Section 9 follow-up), 汇编层已正确 emit 调用。
> - Section 4.2 已知预存限制列表移除 `null deref / float-cast
>   UBSAN trap 变体未实装` 一条 (M25).

> **M25 增量（2026-07-24）— Section 4.2 P1.5 __kcsan_check_access
> 专用路径**：
> - `src/sanitizer.rs::make_mem_check_call` 已在 M15 阶段实装
>   KCSAN-only 分支: 当 `flags.kcsan && !flags.kasan && !flags.hwasan`
>   时, emit `__kcsan_check_access(addr, size, is_write)` 三参
>   调用, 走 LLVM KCSAN 运行时接口而非 KASAN 的 `__asan_loadN`.
>   单元测试 `kcsan_only_uses_kcsan_check_access` 覆盖。
> - P1.5 PR 仅补齐集成层覆盖: 新增
>   `kcsan_only_emits_kcsan_check_access` 集成测试, 用
>   `-fsanitize=thread -S` 编译指针解引用代码, 验证汇编
>   含 `call __kcsan_check_access` 且没有 `call __asan_loadN`
>   / `call __asan_storeN` (证明未走 KASAN 路径).
> - Section 4.2 已知预存限制列表移除 `__kcsan_check_access
>   专用路径未 emit` 一条 (M25).

> **M25 增量（2026-07-24）— Section 6 P2.1 obj.*ptr_to_member
> 解析 + codegen**：
> - `src/cpp/parser.rs` postfix 解析修补: 接受 lexer 产生的
>   `TokKind::DotStar` 单 token (原 M17.0 代码只处理
>   `Dot` + `Star` 两 token 路径, 实际 lexer 从 M17.0 起就
>   直接产 `DotStar`, 导致 `obj.*x` 触发 C0002 "feature
>   not yet supported"). 修补后同时接受 `DotStar` 与
>   `Dot`+`Star` 两种 token 形式, parser 解析为
>   `CppExpr::MemberStar(obj, m)` / `CppExpr::PtrMemberStar(obj, m)`.
> - 配合 `lower.rs` 已有的 MemberStar / PtrMemberStar lower
>   路径 (M17.0 阶段实装), 生成 C 代码
>   `(*(int*)((char*)(obj) + __dcc_mbr_<class>_<member>))`.
> - 新增 fixture `tests/fixtures/cpp/member_ptr_deref.cpp` +
>   集成测试 `p2_1_member_ptr_dotstar`. 集成测试全部 PASS
>   (当前 287 passed, 0 failed, 1 ignored).
> - 运行时限制: 偏移常量 `__dcc_mbr_<class>_<member>` 由
>   lower.rs#L1653 生成完整函数定义 (计算 offsetof), 但
>   该函数需在运行时调用, 非常量表达式。链接时须确保
>   每个 TU 只定义一次 (当前每个 class 定义都会 emit),
>   多文件链接可能触发重复定义错误。本集成测试只验证
>   dcc -c 路径, 完整链接需 host gcc 或添加 weak 属性.
> - Section 6 表格 `obj.*ptr_to_member` 行 不支持 → 🟢 (M25).

> **M25 增量（2026-07-24）— Section 6 P2.2 typeid(e) 真 RTTI 取类型**：
> - 验证 `src/cpp/lower.rs` 中 `CppExpr::Typeid` / `TypeidType`
>   两条 lower 路径 (M17.0 阶段实装) 都生成
>   `(&__dcc_ti_<class>)` 真 RTTI 符号引用:
>   - `Typeid(e)` 动态路径调用 `expr_static_class(e)`
>     推断 e 的静态类, 然后 emit `(&__dcc_ti_<cls>)`.
>   - `TypeidType(ty)` 静态路径对 `MangleType::Class`
>     直接 emit `(&__dcc_ti_<cname>)`, 否则 mangle 后 emit.
> - 原本 M8 简化版 stub `((_ZTI*)(((void*)e)->__vptr + 0))`
>   的 vtable header 假设被替换; 跨多态指针的动态 RTTI
>   仍依赖 dcc C 后端在 vtable 中写 typeinfo 字段,
>   是 Section 9 follow-up.
> - 新增 fixture `tests/fixtures/cpp/typeid_rtti.cpp`
>   (覆盖静态 `typeid(Animal)` 等值比较和
>   动态 `typeid(a)` 取 a 的静态类两条路径) +
>   集成测试 `p2_2_typeid_rtti`.
> - Section 6 表格 `typeid(e)` 行 M8 stub → 🟢 (M25).

> **M25 增量（2026-07-24）— Section 6 P2.3 C++ 模板实参语法 T<U>**：
> - 验证 `src/cpp/parser.rs::parse_mangle_type` (M18 阶段
>   实装) 在 ident 后识别 `<` 触发 `MangleType::Template` 分支;
>   走 `looks_like_template_args_after_lt` 嵌套扫描, 跨过
>   嵌套 `<`/`>`/`>>` 判别模板实参列表. M18 已支持但未加
>   集成测试, 本 P2.3 补上 fixture + 测试.
> - 新增 fixture `tests/fixtures/cpp/template_arg.cpp`
>   (覆盖 `Holder<int>` 单实参 + `Holder<Holder<int>>`
>   嵌套 >> 实参) + 集成测试 `p2_3_template_arg_syntax`.
> - Section 6 表格 `模板实例化` 行 `parse_mangle_type 不识别 <`
>   → 🟢 (M25). 真实单态化仍为 M10+ 简化版 (Section 6 follow-up).

> **M25 增量（2026-07-24）— Section 6 P2.4 operator new[] / delete[] mangler**：
> - `src/cpp/mangle.rs` 新增 4 个命名常量 helper:
>   `mangle_op_new` (`_Znwm`) / `mangle_op_new_array` (`_Znam`) /
>   `mangle_op_delete` (`_ZdlPv`) / `mangle_op_delete_array` (`_ZdaPv`).
>   原 lower 阶段硬编码 `"_Znwm"` / `"_Znam"` / `"_ZdlPv"` / `"_ZdaPv"`
>   字符串全部替换为 helper 调用, 避免 typo.
> - 新增单元测试 `mangle_op_new_constants` 验证 4 个常量
>   名字拼写符合 Itanium C++ ABI 数组分配/释放符号.
> - 新增 fixture `tests/fixtures/cpp/new_arr_mangler.cpp`
>   (覆盖 `Item* arr = new Item[4]; ... delete[] arr;`) +
>   集成测试 `p2_4_new_arr_mangler`.
> - Section 6 表格 `operator new[] / operator delete[]` 行
>   `_Znwm / _ZdlPv 替身` → 🟢 (M25). 符号转发走
>   dccpp_runtime.c extern 声明, 最终在链接时由 libstdc++
>   / libc malloc 解析.

> **M25 增量（2026-07-24）— Section 6 P2.5 new T[n] 数组分配**：
> - 验证 dcc parser 的 `parse_new` 在 `new T[n]` 形式下识别
>   `[` 触发 `CppExpr::NewArr { ty, count }` (M17.0 阶段实装),
>   不再降级为 `new T` 标量分配 (M8 简化).
> - lower 阶段调用 mangle_op_new_array() (P2.4 helper),
>   emit `((T*)_Znam(sizeof(T) * count))` 形式, 对类类型
>   T 用 `(struct T*)` cast, 内建类型直接 `(T*)`.
> - 新增 fixture `tests/fixtures/cpp/new_array_alloc.cpp`
>   (覆盖类类型 Cell + 变量计数 n + 完整 new T[n] / delete[]
>   array 生命周期) + 集成测试 `p2_5_new_array_alloc`.
> - Section 6 表格 `new T[n]` 行 `不识别 [] 按 new T 处理`
>   → 🟢 (M25).

> **M25 增量（2026-07-24）— Section 6 P2.6 placement new**：
> - 验证 dcc parser 的 `parse_new` 在 `new (ptr) T(args)`
>   形式下识别开括号 + 指针表达式触发
>   `CppExpr::PlacementNew { ptr, ty, args }` (M17.0 阶段实装).
> - lower 阶段 emit `((void*)(ptr), ctor_mangled(ptr, args))`
>   走原地构造链, 不调用 operator new 分配新内存, 模拟
>   Itanium C++ ABI placement new 语义. 释放由用户管理
>   (不 emit `delete p`, 调 dtor 后手动 free).
> - 新增 fixture `tests/fixtures/cpp/placement_new_ctor.cpp`
>   (覆盖 `new (p) Token` 无参 + `new (p) Token(42)` 带参
>   两种形式) + 集成测试 `p2_6_placement_new`.
> - Section 6 表格 `placement new` 行 `降级为裸 malloc 替身`
>   → 🟢 (M25).
>
> **M25 增量（2026-07-24）— Section 2 P3.1 zstd 压缩 debug sections**：
> - `src/link/link_opts.rs::compress_debug_sections(layout, "zstd")`
>   走 `zstd::encode_all(level 3)` + ELF 压缩头
>   (ch_type=2 `ELFCOMPRESS_ZSTD`, ch_size=原始大小,
>   ch_addralign=对齐) (M24 阶段实装, 本 P3.1 补单元测试).
> - CLI `-Wl,--compress-debug-sections=zstd` 在 `src/cli.rs`
>   已接受 (M24), 通过 LinkerOptions 传入 link 阶段.
> - 新增单元测试
>   `compress_debug_sections_zstd_marks_flag_and_payload`:
>   构造 .debug_info 节填 4096 字节全 0, 调
>   `compress_debug_sections(&mut layout, "zstd")` 后
>   验证 SHF_COMPRESSED (0x800) 被设置, 段数据前 4 字节
>   等于 [2, 0, 0, 0] (ELFCOMPRESS_ZSTD little-endian),
>   段数据大小在压缩后变小 (zstd 对全 0 输入极致压缩).
> - Section 2 表格 `zstd 压缩 debug sections` 行
>   `仅支持 zlib, zstd 待后续` → 🟢 (M25).
>
> **M25 增量（2026-07-24）— Section 11 P4.1 pragma STDC FENV_ACCESS**：
> - 从 `preproc.rs::process_directive` 抽出独立的
>   `handle_pragma_stdc(text, line_no)` 函数, 集中处理 C99
>   标准的三个 STDC 子句: FENV_ACCESS / FP_CONTRACT /
>   CX_LIMITED_RANGE, 都接受 ON / OFF / DEFAULT 三态, 且
>   状态名不区分大小写.
> - FENV_ACCESS 路径: ON / DEFAULT 置 `self.fenv_access=true`,
>   OFF 置 false, 拼写错 (如 `MAYBE`) 发 W0001 警告 (不
>   静默接受, 避免污染语义状态).
> - FP_CONTRACT / CX_LIMITED_RANGE 走静默 noop 路径 (dcc
>   codegen 暂不区分这两个开关, 与现有 GCC 行为一致).
> - 直接 `#pragma` 形式与 `_Pragma("...")` 操作符两条路径
>   都经 `process_directive` 统一汇入, 复用同一 handler.
> - 运行时 fenv 调用 (fesetround / fegetround 等) 仍
>   未提供, 留 Section 9 follow-up; codegen 也不插
>   `volatile` (FENV_ACCESS 在 dcc 仍为纯语义状态).
> - 新增 3 个单元测试:
>   `pragma_stdc_fenv_access_accepted_silently` 验证 ON/OFF/
>   DEFAULT/小写 都不发警告; `pragma_stdc_fenv_access_unknown_state_warns`
>   验证 `MAYBE` 触发 W0001 警告 (非错误);
>   `pragma_stdc_fp_contract_cx_limited_accepted` 验证
>   FP_CONTRACT / CX_LIMITED_RANGE 静默 noop.
> - Section 11 表格 `浮点环境 (fenv.h)` 行
>   `pragma STDC FENV_ACCESS 解析为注释` → 🟢 (M25).
>
> **M25 增量（2026-07-24）— Section 4.2 增量预处理 (pch) 文档同步**：
> - 调查发现 `src/preproc.rs` 已实现完整 PCH 路径:
>   `PchOptions` (CLI 选项) + `PchBundle` (序列化的宏表 +
>   输出文本快照) + `serialize` / `deserialize` (魔数
>   `DCC_PCH_V1` 头) + `apply` (注入到 Preprocessor 状态) +
>   `hash_macro_state` 驱动的缓存失配检测 (7 个单元测试
>   `pch_bundle_kv_escape_handles_newlines_and_tabs` /
>   `pch_bundle_roundtrip` / `pch_incompatible_macro_state_warns_and_falls_back`
>   / `pch_missing_file_emits_error` /
>   `pch_magic_mismatch_rejected` /
>   `pch_no_pch_does_not_create_file` /
>   `pch_create_then_include_roundtrip` 全部 0 失败).
> - CLI 端 `src/cli.rs` 已接受三个标志:
>   `-fpch-include=<file>` 加载已有 PCH;
>   `-fpch-create=<file>` 预处理成功后写出 PCH;
>   `-fpch-include-only=<file>` 加载并立即 dump 输出 (调试).
> - 文档 M25 之前该行误标 `🔴 无`, 本增量补成
>   🟢 (M25). 实际上 PCH 的端到端功能早已交付
>   (M19 前后, 但当时未同步到本表格).
> - 新增集成测试 `pch_create_then_include_compiles_and_runs`:
>   写一个 `header.h` 包含宏/typedef, preamble.c
>   引用头并 -fpch-create 生成 PCH, main.c 通过
>   -fpch-include 加载 PCH 后编译/链接/运行,
>   验证返回码 0; 同时跑一条无 PCH 对照路径
>   同样返回 0, 验证两条路径在功能上等价.
> - Section 4.2 表格 `增量预处理 (pch)` 行 `🔴 无` → 🟢 (M25).
>
> **M25 增量（2026-07-24）— AArch64 TLSDESC 序列单元测试**：
> - 验证 `src/codegen/aarch64.rs::Aarch64Codegen::emit_tlsdesc_access`
>   走标准 4 指令序列:
>   `adrp x0, :tlsdesc:sym` +
>   `ldr  x1, [x0, :tlsdesc_lo12:sym]` +
>   `add  x0, x0, :tlsdesc_lo12:sym` +
>   `blr  x1` (调 glibc 提供的 `_dl_tlsdesc_resolve`).
> - 序列伴随 3 个重定位
>   (R_AARCH64_TLSDESC_{ADR_PAGE21, LD64_LO12, ADD_LO12}),
>   全部引用同一 TLS 变量符号 (由 `ensure_sym` 去重保证).
> - 新增单元测试 `aarch64_tlsdesc_access_emits_expected_sequence`:
>   构造空 codegen, 调 `emit_tlsdesc_access("my_tls_var")`, 验证
>   code 缓冲里能找到 adrp/ldr/add/blr 4 条指令 (用 aarch64
>   机器码的 top-byte 0x90/0xf9/0x91/0xd6 模式匹配), 且
>   relocations 数组里有且仅有 3 条 TLSDESC_* 类型, 全部
>   引用同一个 sym "my_tls_var".
> - ELF 常量里还预留了 5 个扩展 TLSDESC 重定位
>   (R_AARCH64_TLSDESC_{OFF_G1, OFF_G0_NC, LDR, ADD, CALL}),
>   用于 2018+ 优化版 2 指令 TLSDESC 序列, codegen 暂未
>   用到, 留 Section 9 follow-up.
> - 运行时 `_dl_tlsdesc_resolve` 桩由 host glibc 提供, dcc
>   自身不重新实现.
> - Section 11 表格 `TLS dtv / TLS 描述符` 行
>   `仅基础 tpoff 模型; R_TLSDESC_* 未实装` → 🟢 (M25).
>
> **M25 增量（2026-07-24）— AArch64 va_list / va_arg 帧布局**：
> - `src/ir.rs::ExprKind` 新增 `VaArg { ap: String }` 变体:
>   原先 `Expr::VaArg` 降级到 `ExprKind::Call { children: [ap] }`,
>   与零参函数调用 `f()` 在 IR 层无法区分 (都是单 Local 子节点);
>   新变体携带 ap 名字, 让后端可以明确判别变参读取路径.
>   对应 `src/link/lto_bitcode.rs` 加 tag 16 serialize/deserialize
>   路径 (在尾部, 不影响已有 0..15 tag 的 bitcode 兼容性).
> - `src/codegen/aarch64.rs`:
>   - 增 `is_current_func_variadic` / `va_save_base_off` / `va_offs` 三字段.
>   - 增 `str_xt_sp` / `ldr_xt_sp` 两个 64 位通用寄存器
>     store/load 帮助函数 (编码
>     `str xN, [sp, #off]` = 0xF9000000 | (off/8)<<10 | 31<<5 | N).
>   - `gen_function` 遇 `ast.is_variadic=true`:
>     (1) `stack_bytes += 64` 预留 x0..x7 保存区;
>     (2) 入口按 8 字节步进发出 8 条 `str xN, [sp, #off]`
>         保存 x0..x7 到保存区 (后续 `va_arg` 直接从保存区读).
>   - 增 `gen_va_start(ap)`: 计算 va_save_base 地址到 x9,
>     8 字节 str 到 ap 局部 (ap_off 必须 8 字节对齐).
>   - 增 `gen_va_arg_int(ap)`: 从保存区顺序读 `ldr w0, [sp, #off]`,
>     `va_offs` 步进 8. 超出 8 个槽位发警告并返回 0.
>   - 增 `gen_va_arg_float(ap)`: 暂走 stub (v0..v7 未保存),
>     返回 0.0 + 警告, 留 Section 9 follow-up.
>   - `gen_call` 增 `__builtin_va_start` / `__builtin_va_end` /
>     `__builtin_va_copy` 名字识别, 不发出实际 `bl` 调用,
>     转调对应 gen_va_* 帮助函数.
>   - `gen_expr` / `gen_float_expr` 增 `ExprKind::VaArg { ap }` 分支
>     分别路由到 int / float 路径.
> - 增 2 个单元测试:
>   - `aarch64_va_save_area_stores_x0_through_x7`:
>     验证手动调 8 次 `str_xt_sp` 后 code 缓冲里有 8 条
>     STR X (top byte 0xF9) 且 imm12 与 (16 + i*8) 匹配.
>   - `aarch64_va_arg_int_reads_sequentially_from_save_area`:
>     验证 3 次 `gen_va_arg_int` 发出 3 条 LDR W 指令,
>     偏移按 0/8/16 递增, 且 `va_offs` 推进到 3.
> - 限制: 当前实现仅支持前 8 个 GP 变参槽 (AAPCS64 x0..x7),
>   浮点变参 (v0..v7) 返回 0.0 + 警告, 栈溢出变参 (>8 个)
>   返回 0 + 警告。完整实现需保存浮点寄存器并处理栈溢出。
> - Section 11 表格 `stdarg.h va_arg 跨平台` 行
>   `🟠 x86-64 SysV 完整; aarch64 / riscv64 简化` →
>   `🟠 x86-64 SysV 完整; aarch64 GP 槽 (x0..x7) 走保存区
>   + 顺序读, 浮点 (v0..v7) 暂未保存; riscv64 简化`.
>
> **M25 增量（2026-07-24）— RISC-V va_list / va_arg 帧布局**：
> - `src/codegen/riscv64.rs`:
>   - 增 `is_current_func_variadic` / `va_save_base_off` / `va_offs` 三字段.
>   - `gen_function` 遇 `ast.is_variadic=true`:
>     (1) `stack_bytes += 64` 预留 a0..a7 保存区;
>     (2) 入口按 8 字节步进发出 8 条 `sd aN, off(sp)` 保存
>         a0..a7 到保存区 (用既有 `emit_sd` 帮助函数).
>   - 增 `gen_va_start(ap)`: `addi a0, sp, va_save_base_off`
>     计算保存区地址, `sw a0, ap_off(sp)` 写入 ap 局部.
>   - 增 `gen_va_arg_int(ap)`: 从保存区顺序读 `ld a0, off(sp)`
>     (用既有 `emit_ld` 帮助函数), `va_offs` 步进 8.
>     超出 8 个槽位发警告并返回 0.
>   - 增 `gen_va_arg_float(ap)`: 暂走 stub (fa0..fa7 未保存),
>     返回 0.0 + 警告, 留 Section 9 follow-up.
>   - `gen_call` 增 `__builtin_va_start` / `__builtin_va_end` /
>     `__builtin_va_copy` 名字识别, 不发出实际 `jal` 调用,
>     转调对应 gen_va_* 帮助函数.
>   - `gen_expr` / `gen_float_expr` 增 `ExprKind::VaArg { ap }` 分支
>     分别路由到 int / float 路径. 这两个函数同时把
>     `match expr.kind` 改为 `match &expr.kind` 以借用
>     `VaArg { ap: String }` 里的 String 字段.
> - 增 2 个单元测试:
>   - `riscv64_va_save_area_stores_a0_through_a7`:
>     验证手动调 8 次 `emit_sd` 后 code 缓冲里有 8 条
>     SD (LE byte0 & 0x7f == 0x23, byte1 & 0x70 == 0x30).
>   - `riscv64_va_arg_int_reads_sequentially_from_save_area`:
>     验证 3 次 `gen_va_arg_int` 发出 3 条 LD 指令
>     (LE byte0 & 0x7f == 0x03, byte1 & 0x70 == 0x30),
>     且 `va_offs` 推进到 3.
> - 限制: 当前实现仅支持前 8 个 GP 变参槽 (RISC-V a0..a7),
>   浮点变参 (fa0..fa7) 与栈溢出变参 (>8 个) 暂未实装,
>   留 Section 9 follow-up.
> - Section 11 表格 `stdarg.h va_arg 跨平台` 行
>   `🟠 x86-64 SysV 完整; aarch64 GP 槽 (x0..x7) 走保存区
>   + 顺序读, 浮点 (v0..v7) 暂未保存; riscv64 简化` →
>   `🟠 x86-64 SysV 完整; aarch64 GP 槽 (x0..x7) 走保存区
>   + 顺序读, 浮点 (v0..v7) 暂未保存; riscv64 GP 槽 (a0..a7)
>   走保存区 + 顺序读, 浮点 (fa0..fa7) 暂未保存`.

> **M21 增量（2026-07-21）**：`src/debug.rs` 顶部加
> `#![allow(non_upper_case_globals)]` 抑制 121 个
> `DW_TAG_*` / `DW_AT_*` / `DW_FORM_*` / `DW_CFA_*` /
> `DW_LNS_*` / `DW_LNE_*` / `DW_LNCT_*` / `DW_INL_*`
> 命名风格告警（这些常量严格沿用 DWARF v4 规范原名,
> 改写为 ALL_CAPS 会丢失与规范章节对应）。`cargo build`
> 与 `cargo build --tests` 0 警告。
>
> `src/cpp/driver.rs` 移除硬编码 `D:\git\dcc\target\debug-cpp-preproc.c`
> debug 写文件（原 M18 调试期产物，本机调试期间已不再需要），并
> 恢复调 dcc 编译后 `fs::remove_file(&tmp)` 清理临时 `.c` 的行为，
> 避免在 `/tmp` 累积 `dcc-cpp-*.c`。

### 8.2 诊断（`src/diagnostics.rs` + `tests/errors.rs`）

✅ E0001-E0013 / E0100-E0102 / E0200-E0209 / E0300 / E1001-E1010 / W0001 / W0200

🟢 已覆盖 snapshot（2026-07-21 更新）：

- E0101 (cannot write) — `e0101.stderr.snap`（写不可写路径触发）
- E0102 (cannot invoke assembler) — `e0102.stderr.snap`（非法指令 `as` 失败）
- E0300 (SSE 与浮点冲突) — `e0300.stderr.snap`（`-mno-sse` + 浮点字面量）
- E1001-E1010 全部 — `e1001.stderr.snap` … `e1010.stderr.snap`
- E0205/E0209（Unix 权限拒绝）— `e0205.stderr.snap` / `e0209.stderr.snap`
- E0208 UTF-8 / `#embed` 找不到 — `e0208_utf8.stderr.snap` / `e0208_embed.stderr.snap`
- W0001 隐式 int — `w0001.stderr.snap`
- `diagnostic_format.stderr.snap`（E0004 canonical 格式锁定）

🟢 诊断选项（2026-07-21 全部实装）：

- `-fdiagnostics-color=always/never/auto`（auto 走 `IsTerminal` 判定）
- `-fdiagnostics-format=github/clang/msvc/vi/sarif`（sarif 写 stderr JSON 2.1.0）
- `-fdiagnostics-show-template-tree` / `-fno-diagnostics-show-template-tree`

回归覆盖（`tests/errors.rs::diag_*`）：

| 测试 | 覆盖点 |
|------|--------|
| `diag_color_never_strips_ansi` | never 模式无 `\x1b[` |
| `diag_color_always_emits_ansi` | always 模式有 `\x1b[` |
| `diag_color_rejects_unknown_value` | 非法值 → `dcc: error:` + exit 2 |
| `diag_format_clang_strips_brackets` | clang 模式首行 `error: …` |
| `diag_format_msvc_uses_file_line_col` | msvc `file(line,col): error Exxxx:` |
| `diag_format_vi_is_single_line` | vi 单行 |
| `diag_format_sarif_emits_json` | sarif `version: 2.1.0` + `name: dcc` + `ruleId` |
| `diag_format_rejects_unknown_value` | 非法 format 值 → `dcc: error:` + exit 2 |
| `diag_template_tree_runs_without_panic` | 开关被接受且不破坏现有诊断 |

### 8.3 Sanitizer / LTO / Profiling

🟢 / 🟠 / 🔴 状态（2026-07-21 M20 之后）：

- **PGO instrument**（`-fprofile-generate` / `-fprofile-use`） 🟢
  - `ProfileConfig.pgo_generate` / `pgo_use` 开关齐
  - `Profraw` 二进制 roundtrip 完成（PROF magic + version 1 + sites
    + u64 计数器数组）
  - `pgo::instrument_program` 在 AST 层 FuncDecl 顶部插入
    `__dcc_pgo_count_fn(idx)` 调用
  - ✅ (M24) driver.rs 接入 `pgo::instrument_program`, codegen 阶段
    自动 emit `call __dcc_pgo_count_fn` 指令
  - `PgoWeights` 聚合 func 入口计数，备查
  - 已知限制: 下游权重传播 stub
- **GCOV**（`-ftest-coverage` / `-fprofile-arcs`） 🟢
  - `.gcno` notes + `.gcda` 数据 二进制格式 (与 GCC gcov-9 layout 对齐)
  - `GcovUnit` / `GcovFunction` / `GcovBlock` 数据结构
  - `write_notes` / `write_data_with_counters` 写盘入口
  - 🟢 (M24) `cfg_checksum` 采用 GCC libgcov `compute_cfg_checksum` 公式:
    对每条 succ arc 加 `(dst_index + 1)`, CFG 拓扑敏感
  - 已知限制: 非 x86_64 arch 未验证
- **AutoFDO** 🟢
  - 简化 text format parser（`func <name> <total>` / `line <name>
    <line> <col> <samples>`）
  - `bucketize` 按 >10% / >1% 阈值分 Hot/Warm/Cold
  - ✅ (M25) 通过 `ProfileWeights` 聚合，hot 函数下沉到 `.text.hot` 段
- **LBR-based profiling**（`-flbr-profile=`） 🟢
  - 解析 `perf script -F brstack` 文本（`from_func from_addr
    to_func to_addr cycles`）
  - `hot_entries` 按 `unknown -> func` 入度排序
  - ✅ (M25) 通过 `ProfileWeights` 聚合，hot 函数下沉到 `.text.hot` 段
- **MemProf / Page Weight Profiler**（`-fmemory-profile` /
  `-fmemory-profile-use=`） 🟢
  - 简化 format parser（`alloc <frames...> = <count> <bytes>` /
    `lifetime <alloc> -> <dealloc> = <bytes>`）
  - `is_cold(stack, threshold)` 启发
  - ✅ (M25) 冷分配栈对应的函数下沉到 `.text.unlikely` 段
- **跨源权重聚合**（`-fprofile-use` / `-fauto-profile` /
  `-flbr-profile` / `-fmemory-profile-use` 任意组合） 🟢 (M25)
  - 新增 `profile::weights::ProfileWeights` 模块：聚合 4 源 PGO /
    AutoFDO / LBR / MemProf，输出统一 hot/cold func 集合
  - 新增 `FuncDecl.is_hot: bool` 字段；`x86_64` codegen 把 hot 函数
    切到 `.text.hot`，cold 函数切到 `.text.unlikely`（与
    `-fprofile-sections` 协同）
  - driver 在 PGO 插桩之后、IR lower 之前调用
    `apply_profile_weights(&mut prog, &cfg.profile)`，任意源文件
    读取失败都 silently 跳过（与 GCC/Clang 行为一致）
- **Perf event 输出 (TopdownL1 metrics)**（`-ftopdownmetrics`） 🟢 (M25)
  - `TopdownL1` 枚举 + `PerfRecord` 容器
  - `topdown_percentages()` 把四 slot 规整为百分比
  - ✅ (M25) 新增 `profile::perf_data::PerfDataFile` 读 Linux
    `perf.data` v2 二进制（`PERFILE2` magic + header + perf_event_attr
    数组 + data section），不依赖 `libperf` 运行时
  - `event_names()` / `synthetic_counts()` 暴露元信息给上层 metric 汇总
- **Offloading (OpenMP target / CUDA / HIP)** 🟢
  - `OffloadConfig` / `OffloadSection` / `OffloadEntry` 数据结构
  - `OffloadKind` 区分 OmpTarget / OmpTargetSimd / Cuda / Hip
  - CLI 全形式解析 + 值校验（`-fopenmp[=...]` / `-fcuda` / `-fhip` /
    `-fopenmp-targets=` / `--offload-arch=`，非法值报错带具体值）
  - `.dcc.offload` 二进制序列化（magic `DCC_OFFL` + version 1 +
    entries 含 col/arch）+ `to_json()` 诊断输出
  - preproc 收集 `#pragma omp`；driver 收集 entries（E0310 /
    W0311-13 均带精确 file:line:col）；codegen 全 arch 输出
    `.dcc.offload` ELF 节
  - ✅ Device codegen: PTX (NVIDIA) / AMDGCN (AMD) 汇编生成 (`profile::device_codegen`)
  - 已知限制：runtime loader 未实装，device 代码为 stub 实现

详见 `docs/profiling.md`。

---

## 9. 自举 / 跨编译 / Bootstrap

### 9.1 自举 M5

🟠 / 🔴 状态：

- dcc 源码量 ~5 万行 Rust；自举尚未开始
- 步骤 1 (`rustc → dcc0`)：✅
- 步骤 2 (`dcc0 → dcc1`)：未开始（需要 dcc0 完整支持自身依赖：std 子集）
- 步骤 3 (`dcc1 → dcc2`)：未开始
- 🟢 (M25) 字节级 / 语义级回归对比基础设施 (`src/bootstrap.rs`):
  - 三层对比: byte (同节逐字节, 限 16 个不同偏移) / struct (节集合+类型+体积) /
    semantic (全局符号集合+大小+bind+引用关系)
  - CLI 开关 `--bootstrap-compare=<ref.o>`, 编译结束后, 把当前输出
    .o 与 ref.o 做三层对比, 仅诊断打印, 不阻塞编译 (与 GCC
    `-fcompare-debug` 行为一致)
  - 9 个单元测试覆盖: identical / byte-diff / struct-diff /
    symbol-add/remove/size/bind / invalid-elf / format

### 9.2 交叉编译

✅ `--target=x86_64/aarch64/riscv64/mips/mips64/arm` 有完整后端

🟢 (M25) Musl sysroot 集成:
- 新增 `--sysroot=<path>` CLI 开关 (也接受 `-Wl,--sysroot=`)
- driver 最早期 `apply_sysroot()` 重写 `cfg.include_paths`,
  前缀 `<sysroot>/include` 与 `<sysroot>/usr/include`
- 新增 `TargetTriple::musl_loader_path()`: 当 abi == Musl,
  返回 musl 动态加载器在 sysroot 内的相对路径 (例: x86_64
  → `lib/ld-musl-x86_64.so.1`); mips 走保守 None
- 7 个单元测试覆盖各 arch (x86_64/aarch64/riscv64/arm) + mips 拒绝
  + gnu 返回 None + `--sysroot=` 解析

🔴 缺：

- 真正的多平台 binary（`x86_64-dcc` / `aarch64-dcc` / `riscv64-dcc`）
- Windows MSVC 目标（`windows-msvc`）— codegen 仍是 SysV-like，Windows 本地无法完整验证链接路径 (依赖 host ld / objcopy，Windows 无等效工具链)
- Darwin 目标 — 没有 Mach-O 输出
- WASM 目标 — 没有 wasm32 codegen
- AVR / MSP430 / Xtensa — 没有
- PowerPC — 没有
- LoongArch64 — 没有

### 9.3 Bootstrap

🟠 / 🔴：

- GCC bootstrap：M9 解锁 dcc++ 端到端；M10+ 增强 SFINAE/partial spec/concept/coroutine/module
- cc1/cc1plus 编译：GCC 源 ~1G + 构建 ~5G, 需本地大磁盘（已具备, 后续 M 推进）
- libiberty：本地 125/0/4（125 PASS / 0 FAIL / 4 SKIP）
- libdecnumber：通过 `tools/libdecnumber_combined.c` 组合 TU
- fixincludes：本地 6/0/1
- binutils 工具链：未尝试（strip / objdump / objcopy / ld 由 host binutils 提供）

---

## 10. 链接器 / 动态加载

🟢 **M22 已完成**（2026-07-21）：

- ✅ `-Wl,--dynamic-linker=` 自定义解释器（PT_INTERP + .interp 节）
- ✅ `-z relro` / `-z now` / `-z noexecstack` 段处理
- ✅ `-z max-page-size` / `-z common-page-size`
- ✅ IFUNC 多版本 dynamic resolver（`target_clones` 实际生成 STT_GNU_IFUNC）
- ✅ `-flto-partition=` 配置化（LtoConfig 结构体）

🟡 (M25) vDSO 集成基础设施:
- 新增 `link::vdso::VdsoInfo` + `parse_vdso()`: 读 vDSO ELF, 提取
  `.text` 载荷 + `.dynsym` (仅 STT_FUNC) + `.strtab`
- `vdso_alias(standard_name)` 辅助: `clock_gettime` →
  `__vdso_clock_gettime` 等 4 个内置别名
- 7 个单元测试覆盖: 解析 .text + 符号、bad magic、truncated header、
  ELF32 拒绝、no .text 拒绝、alias 映射、文件读取端到端
- 未完成: 把 vDSO `.text` 嵌入输出 layout + 加 `.note.GNU-vdso` 节.
  需要重写 `link::layout` 的段追加顺序, 留作下一阶段.

🟡 (M25) DSO symbol caching:
- 新增 `link::dso_cache::DsoCache`: 按 (path, mtime, size) 三元组去重
  缓存 DSO 解析结果, 返回 `Arc<DsoInfo>` 共享所有权
- 便捷入口 `get_or_load(cache, path, loader)`: 命中走缓存, 未命中
  调用 loader 并 insert
- 5 个单元测试覆盖: miss-then-hit、size 变化时失效、loader 仅调用
  一次、clear 后清空、缺失路径返回 None
- 未完成: 链接器侧接入 (link/mod.rs 当前还没有 DSO 读取路径, 需要
  先把 built-in linker 的 -lxxx → libc.so 解析补上, 再用 DsoCache
  做缓存层)

🔴 仍缺：

- 运行时 dynamic loader（`ld.so`）— 完全依赖 host（设计决策）

---

## 11. 杂项 / 边角

🟢 / 🟠 / 🔴 状态（2026-07-24 M24 之后）：

| 模块 | 状态 |
|------|------|
| `setjmp` / `longjmp` | 🟢 (M24) 走 libc + 生成 DWARF CFI 兼容的寄存器保存表（CIE/FDE 修复 + DW_CFA_def_cfa + DW_CFA_offset） |
| `alloca` | 🟢 (M23) `__builtin_alloca` codegen: sub rsp + 16 字节对齐 |
| 信号 | 🟢 (M25) `sigaction` / `sigaltstack` / `signal` 在 link implicit undef 白名单; signal() / sigaction() / sigaltstack() 运行时验证通过 |
| TLS dtv / TLS 描述符（`gottpoff` / `tlsdesc`） | 🟢 (M25) AArch64 `emit_tlsdesc_access` 发出 adrp + ldr + add + blr 序列 + 3 个 R_AARCH64_TLSDESC_{ADR_PAGE21, LD64_LO12, ADD_LO12} 重定位, `emit_tls_ie_access` / `emit_tls_le_access` 走 Initial-Exec / Local-Exec 路径; ELF 常量里已定义扩展 TLSDESC 5 个 (R_AARCH64_TLSDESC_{OFF_G1, OFF_G0_NC, LDR, ADD, CALL}) 预留, 但 codegen 还未用到 (Section 9 follow-up); 运行时 `_dl_tlsdesc_resolve` 依赖 host glibc, dcc 自身不提供桩 |
| atexit / `__cxa_atexit` | 🟢 (M22) `fini_dispatch.rs::FiniArrayBuilder` 按 priority 排序 |
| `__attribute__((constructor(priority)))` | 🟢 (M23) 按 priority 排序（小数字先执行，默认 65535） |
| `__attribute__((destructor(priority)))` | 🟢 (M23) 按 priority 排序 |
| `__attribute__((alias("target")))` | 🟢 (M23) 复用 `alias_name` 通道生成别名符号 |
| `__attribute__((ifunc("resolver")))` | 🟢 (M23) 解析并存为 `ifunc_resolver`，codegen 生成 STT_GNU_IFUNC |
| `__attribute__((retain))` | 🟢 (M23) 标记节 SHF_GNU_RETAIN (0x200000) |
| `__attribute__((used))` | 🟢 (M23) 标记 USED，防止 gc-sections 移除 |
| `__attribute__((flatten))` | 🟢 (M23) 所有调用视为 always_inline |
| `__attribute__((error("msg")))` / `warning("msg")` | 🟢 (M23) 调用时发射编译错误/警告诊断 |
| `__attribute__((deprecated("msg")))` | 🟢 (M23) 调用时发射弃用警告 |
| `__builtin_unreachable()` | 🟢 (M23) 发射 ud2 + 标记不可达路径 |
| `__builtin_trap()` | ✅ ud2 |
| `__builtin_debugtrap()` | 🟢 (M23) 发射 int3 (0xCC) |
| `__builtin_assume_aligned(p, n)` | 🟢 (M23) 透传指针，codegen 优化对齐 |
| `__builtin_expect_with_probability` | 🟢 (M24) 解析层与 `__builtin_expect` 一致处理: 字面量首参编译期折叠, 其它形态走 codegen 既有 builtin 路径 |
| 浮点环境 (`fenv.h`) | 🟢 (M25) `pragma STDC FENV_ACCESS ON/OFF/DEFAULT` 走 `handle_pragma_stdc` 独立函数, ON/DEFAULT 置 `fenv_access=true`, OFF 置 false, 小写状态也接受; 未知状态 (如 `MAYBE`) 发 W0001 警告; FP_CONTRACT / CX_LIMITED_RANGE 静默 noop; codegen 阶段不插 `volatile` (仍是纯语义状态) — 运行时 fenv 调用留 Section 9 follow-up |
| 复杂数学函数 (`cproj`, `cabs` 等) | 🟢 (M25) cproj / cabs / carg 走 libm: link 隐式 undef 白名单 + fixture + integration 覆盖 (Section 11 P0.3); 真实 `double complex` 传值 ABI 留 Section 9 follow-up |
| `printf` format string 全集 | 🟢 (M23) 完整 flag/width/precision/length/type 匹配 |
| `scanf` format string | 🟢 (M23) 完整格式检查 + 指针类型验证 |
| `wchar_t` | 🟢 (M25) 解析为 int; 补齐 wcs*/wmem* libc 转发: wcscpy/wcscat/wcscmp/wcsncmp/wcslen/wcschr/wcsrchr/wmemcpy/wmemmove/wmemset/wmemcmp 在 link 隐式 undef 白名单, fixture+integration 覆盖 (Section 11 P0.1) |
| 宽字符字面量 `L"..."` | 🟢 (M23) 解析为 wchar_t 编码字符串 |
| `char16_t` / `char32_t` (C11/C++11) | 🟢 (M23) 解析为 uint16/uint32 + u""/U"" 字符串 |
| 原子混合大小（`atomic_load_explicit(_Atomic short*, ...)`） | 🟢 (M24) `__atomic_*` codegen 补齐 1/2 字节 (x86-64 编码: lock xadd/xchg/cmpxchg 8/16 位变体 + sil/si/dx/dl 子寄存器 + movzx 零扩展返回值) |
| `stdatomic.h` 全部 (`atomic_flag` / `atomic_signal_fence`) | 🟢 (M24) `atomic_signal_fence` 走 `__asm__ volatile("" ::: "memory")` 编译器屏障; `atomic_thread_fence` 同实现; `atomic_flag` 走现有 builtin 路径 |
| `stdarg.h` va_arg 跨平台 | 🟠 x86-64 SysV 完整；aarch64 GP 槽 (x0..x7) 走保存区 + 顺序读，浮点 (v0..v7) 暂未保存；riscv64 GP 槽 (a0..a7) 走保存区 + 顺序读，浮点 (fa0..fa7) 暂未保存 |

---

## 12. 已显式归档不做（"明确不做"）

`docs/next_steps.md` 第 134–136 行已声明：

- ~~完整 GCC bootstrap~~：GCC 主体为 C++，无 `dcc++` 前端（M9/M10+ 部分解锁）
- ~~libdecnumber Rust 移植~~：Makefile 组合 TU 即可（已完成）
- ~~gen* 工具重写~~：服务于 GCC 构建管线

`docs/architecture.md` 末尾也归档了：

- 运行时 dynamic loader（完全依赖 host ld.so，设计决策）
- vDSO 集成

`docs/missing_features.md` 第 215–217 行同样指出：

> 明确不做：运行时 dynamic loader（依赖 host ld.so，设计决策）

---

## 13. 优先级建议（M12+ backlog）

按 ROI 从高到低：

1. ✅ **x86_64 mini-asm 补全**（高）— M12 已完成：消除 nop 警告，覆盖内核 paravirt / 启动代码。
2. **KASAN/UBSAN 指令级插桩**（高）：把 sanitizer 从桩升级为真功能。
3. **C++ 模板单态化**（高）：让 `std::vector<int>` / `std::map<K,V>` 真正工作。
4. **完整 GCC bootstrap**（中）：本地磁盘已具备, 与后续 dcc++ SFINAE / 协程 / module 推进同步.
5. **AArch64 EH_FRAME / ORC / DWARF**（中）：让 aarch64 后端在生产环境可调试。
6. **MIPS / ARM 后端**（中）：解锁更多交叉编译场景。
7. **优化 pass（强度削减 / 调度 / 别名分析）**（低）：性能优化，dcc 本身定位 suckless。
8. **PowerPC / LoongArch / WASM / AVR**（低）：新平台扩展。
9. **PGO / AutoFDO**（低）：性能分析。
10. **自举 M5**（低）：与第 4 项互锁。

---

## 附录 A：相关文档

- `docs/architecture.md` — 整体架构 + 5 万行代码目标
- `docs/roadmap.md` — 里程碑 M0–M11
- `docs/missing_features.md` — 已通过/阻塞级/缺口表
- `docs/next_steps.md` — 20 步路线图
- `docs/c_standard.md` — C 标准支持矩阵
- `docs/diagnostics.md` — 错误码参考
- `docs/gcc_build_guide.md` — GCC bootstrap 指南
- `docs/kernel_build_guide.md` — Linux 内核编译指南

## 附录 B：本地验收脚本

| 脚本 | 用途 |
|------|------|
| `scripts/test_all.sh` | 本地全量 cargo test 一键 |
| `scripts/ci-trigger.sh` | CI 触发 |
| `scripts/kernel_full_check.sh` | modpost / QEMU / vmlinux 编排 |
| `scripts/kernel_full_build.sh` | 内核全树构建 |
| `scripts/kernel_build_smoke.sh` / `kernel_phase_b_check.sh` | smoke / phase B |
| `scripts/prepare0_check.sh` / `prepare0_full.sh` | prepare0 窄/全链路 |
| `scripts/modpost_link.sh` | modpost 链接 + fixdep |
| `scripts/pahole_regress.sh` / `objtool_regress.sh` / `vmlinux_regress.sh` | 回归 |
| `scripts/qemu_boot_matrix.sh` | QEMU 启动矩阵 |
| `scripts/test_kernel_features.sh` | 内核特性示例 |
| `scripts/build_fixdep_dcc.sh` / `gen_elfconfig_h.sh` | 工具链辅助 |
| `scripts/bench.sh` / `bench_phase_e.sh` | 性能与阶段 E 基准 |
| `scripts/dcc-host-wrapper.sh` | host 链接器包装 |
| `scripts/update_error_snapshots.sh` | 刷新 E1xxx snapshot |
| `scripts/archive/` | 历史调试与一次性探针脚本 |

> **M26 增量 — Section 9 _Complex 字面量与 __real__/__imag__ 关键字**：
> - AST 新增 `ComplexLit` / `Real` / `Imag` 三个 Expr 变体
> - 词法分析器支持虚数后缀 `i`/`I`/`j`/`J`（整数与浮点）
> - 关键字 `__real__`/`__real`/`__imag__`/`__imag` 映射到 `KwReal`/`KwImag`
> - 解析器 `__extension__` 表达式前缀特判（`complex.h` 中 `_Complex_I` 宏展开）
> - Codegen 完整实现：ComplexLit 字面量写帧临时区、`__real__`/`__imag__` 分量访问
>   （lvalue 直接寻址，rvalue 从 gen_complex_binop 结果取）、Unary Neg 复数分支
> - `spill_complex_operand` 修复标量→complex 提升（`2.0` 作为 `z * 2.0` 右操作数）
> - 新增 `tests/fixtures/c/complex_literal.c` + 集成测试 `complex_literal_run_returns_zero`
> - 全量 286 集成测试通过，无回归
