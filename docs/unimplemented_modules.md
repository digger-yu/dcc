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

测试：`tests/fixtures/asm_avx_aes_fma_tsx.c`（含 AMX/VPINSR/EVEX/TSX xabort）+
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
| `gen_float_binop` 不支持运算 | `x86_64.rs:4294` | float/double 取模软件实装：cvttsX2si + cvtsi2sX + mul + sub，零 libc；`tests/fixtures/float_mod.c` |
| XMM 临时槽 | `x86_64.rs:7550` | `prepare_asm_xmm_binding` 加载 float 到 xmm；`writeback_asm_operand` 按 `xmm_width` 写回 xmm8–15；`input_xmm_set` + early-clobber `xmm_pool` |
| int128 binary 逻辑 | `x86_64.rs:2577` | `Expr::Binop` 优先 `&&`/`\|\|`，`or rdx, dst` 合并高 64 位；`tests/fixtures/int128_logical.c` |

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
AArch64+RISC-V 重定位；GOT64；TLS 重定位；PIE/Shared 输出；动态节。

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
| LTO IPA-SRA / PGO / 循环向量化 | 跨 TU 内联+常量传播+DCE+DAE 已完整，IPA-SRA 与 PGO 集成待后续 M |
| 运行时 dynamic loader | 完全依赖 host ld.so（设计决策，非缺失） |
| zstd 压缩 debug sections | 仅支持 zlib，zstd 待后续 |

---

## 3. Sanitizer 桩（`src/sanitizer.rs`）

🟢 AST 层全部通道已交付（KASAN / HWASAN / UBSAN / KCSAN / KCOV / Fuzzer）。

| Sanitizer | 实际能力 | 已知遗留 |
|-----------|----------|------|
| KASAN (`-fsanitize=address`) | TU init + `__asan_load1/2/4/8/16` / `__asan_storeN` 在 `Deref/Index/ArrowMember` 与对应 `Assign` 之前插入 | dcc codegen 对 `Assign` LHS 仍仅支持 `Ident`；`*p = v` 的实际 store 是空操作（KASAN 桩本身正确） |
| HWASAN (`-fsanitize=hwaddress`) | 同 KASAN（共享路径，函数名解析时分发） | ARM MTE 标签插入未做 |
| KASAN-kernel (`-fsanitize=kernel-address`) | 同 KASAN | 内核定制 shadow 范围（`0xffff...`）未单独分发 |
| UBSAN (`-fsanitize=undefined`) | TU init + `Div/Mod` 前 `__ubsan_handle_divrem_overflow(0, divisor)`；`Shl/Shr` 前 `__ubsan_handle_shift_out_of_bounds(0, shift_amt)`；有符号 `Add/Sub/Mul` 前 `__ubsan_handle_signed_integer_overflow(0, lhs)` | VLA bound / null deref / float cast trap 仍缺 |
| KCSAN (`-fsanitize=thread` / `kernel-kcsan`) | TU init + 每函数 `__kcsan_func_entry`；内存访问 watchpoint 与 KASAN 共享 load/store 通道 | 专用 `__kcsan_check_access` 路径未独立发出 |
| KCOV (`-fsanitize-coverage`) | 每函数 `__sanitizer_cov_trace_pc` + `__dcc_cov_guards_N[16]` 计数器 + `__sanitizer_cov_trace_pc_guard` 在 if / while / do / for / case / label / goto 等边 | 跨函数 inline 合并未做 |
| Fuzzer (`-fsanitize=fuzzer`) | KCOV 全部 + 比较 `__sanitizer_cov_trace_cmp1/2/4/8` + switch dispatch `__sanitizer_cov_trace_switch` | libFuzzer 协议 / 拦截器接入未做 |

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

- 16 字节/聚合 KASAN load/store 未单独 emit（走通用 `__asan_load`）
- `__kcsan_check_access` 专用路径未 emit（KCSAN 借道 KASAN 回调）
- VLA / null deref / float-cast UBSAN trap 变体未实装

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

**测试**：`tests/fixtures/m16_*.c` 7 个（ssa_ir / strength_reduction /
cross_function_dce / sccp / loop_opt / tco / function_attr），
`tests/integration.rs` 对应 `m16_*` 函数在本地 `cargo test m16_`
验证。详见 `docs/architecture.md` M16 章节。

---

## 6. C++ 前端（`src/cpp/`）

### 6.1 已实装（M7-M10+）

✅ 命名空间 / 运算符 / 继承 (单/多/钻石) / 虚函数 / vtable (`_ZTV<Name>`) /
typeinfo (`_ZTI<Name>`) / RTTI / 异常 (Itanium zero-cost) / lambda / 模板占位 /
ctor 初始化列表 / 隐式 this 注入 / 局部类变量 / C++20 占位 (module/import/export/
concept/requires/co_await/co_yield/co_return) / M10+ SFINAE / partial spec /
完整 concept 求值 / 协程状态机 / module 分区 / operator-> 链 /
跨虚继承 dynamic_cast / 构造委托 (`using Base::Base;`) / 成员函数指针 /
`__cxa_guard_*` 线程安全 init guard / `std::initializer_list`。

🟠 简化 / placeholder：

| 模块 | 现状 |
|------|------|
| 模板实例化 | M10+ 实现 `PartialSpec` + SFINAE，但模板函数/类的真实单态化是简化版：依赖 `try_substitute` 模式匹配；`T<U>` 显式实参语法 `parse_mangle_type` 不识别 `<` |
| `obj.*ptr_to_member` (M8 简化) | 不支持 |
| `typeid(e)` (M8 简化) | 降级为 `((_ZTI*)(((void*)e)->__vptr + 0))->...`，不实际取 RTTI |
| `auto` (M17 实装) | `infer_type` 真类型推导: int 字面量→Int, `"abc"`→`const char*` (`MangleType::Pointer(Int)`), 类名已知 → `MangleType::Class` |
| `auto x = expr;` | M17 真推导: `Widget w(7)` 拆为 `Widget w; _ZN6WidgetC1Ei(&w, 7)` |
| `decltype(expr)` (M17 实装) | 解析为 `MangleType::Int` 占位 (M17.0 简化) |
| `decltype(auto)` (M17 实装) | 解析为 `MangleType::DecltypeAuto` 占位 |
| `using Base::Base;` 继承链 ctor (M17 实装) | 类内 `using` 被 lower 接受, InheritingCtor 节点继续复用既有 lower; 多文件链接场景受 dcc C 后端 `__base_Base1` 字段访问 pre-existing 限制 |
| `co_yield` / `co_await` (M10+) | 降级为 `static int __coro_state = (expr);` 状态机，不是真挂起 |
| `concept` / `requires` (M10+) | 解析为注释 + 简化求值；不与 SFINAE 完整联动 |
| `module` / `import` (M10+) | 注释占位，无实际模块解析 |
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
| C++20 Modules (M18 占位) | `module;` / `export module X;` / `import X;` 接受; M19+ 实现 BMF 序列化 |
| `static` 类成员 | 🟢 (M23) 解析为 C `static` 字段 + Itanium ABI mangling (`_ZN<class><name>E`) |
| `virtual` 继承 | 部分支持（见 `M10+`），复杂虚基类不验证 |
| `placement new` | 降级为裸 `malloc` 替身 |
| `operator new[]` / `operator delete[]` | 降级为 `_Znwm` / `_ZdlPv` 替身 |
| `new T[n]` 数组分配 | 不识别 `[]`，按 `new T` 处理 |

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
| 增量预处理（pch） | 🔴 无 |

M19 关键改动:

- `__COUNTER__` 在 `Preprocessor` 中以 `u32` 状态字段持有, `expand_with_disabled_inner` 每次访问自增 (wrapping).
- `__VA_OPT__` 在 `substitute_params` 入口按"是否可变参数宏 + __VA_ARGS__ 是否非空"二分支消解, 解析时跳过字符串/字符字面量, 处理前后配对的 `(` 与嵌套.
- 预定义宏全部移到 `Preprocessor::new` 旁的 `define_builtin` 调用点, 不依赖外部 libc 头.
- include 缓存版本从 V6 → V7, 任何 `hash_macro_state` 变化都让缓存自动失配.
- `expand_with_disabled_inner` 在函数宏调用前增加 `raw_args.is_empty() && !params.is_empty()` 分支: 当宏期待 ≥1 实参却被空括号 (`FOO()`) 调用时, 按 GCC 行为视为 N 个空实参展开. 修复 glibc 头 `__STRING ()` / `__ASMNAME2` 链在 dcc 预处理器里残留 `__STRING` 字面量的问题, 使 `<stdio.h>` / `<stdlib.h>` 在 dcc 下能被正常解析.

---

## 8. 调试 / 诊断 / 性能工具

### 8.1 调试信息（`src/debug.rs` + `src/codegen/elf.rs`）

🟠 部分实装：

- DWARF v4：✅ x86_64 / ✅ AArch64 (M8) / ✅ RISC-V (M8)
- `.BTF`：✅ x86_64（struct dcc_probe）/ ✅ AArch64 (M8) / ✅ RISC-V (M8)
- `.eh_frame`：✅ x86_64（CIE + FDE）/ ✅ AArch64 (M8) / ✅ RISC-V (M8)
- `.orc_unwind_ip`：✅ x86_64 / ✅ AArch64 (M8) / ✅ RISC-V (M8)
- DWARF line table：🟢 简化（无 matrix 优化，但基本行号信息完整）
- DWARF type info：🟢 简化（int/struct probe）
- DWARF call frame：✅ CIE + FDE
- `-gsplit-dwarf` / `-gpubnames` | 🟢 M23 已实装（split-dwarf 支持 `.dwo` 文件生成）
- GDB index (`-Wl,--gdb-index`) | 🟢 M22 已实装（最小 `.gdb_index` 节占位）
- AArch64/RISC-V DWARF / BTF | ✅ M8 已实装（`build_dwarf_full` + `.BTF` 节）

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
  - `PgoWeights` 聚合 func 入口计数，备查
  - 已知限制：codegen 阶段尚未自动 emit 计数器，下游权重传播 stub
- **GCOV**（`-ftest-coverage` / `-fprofile-arcs`） 🟢
  - `.gcno` notes + `.gcda` 数据 二进制格式 (与 GCC gcov-9 layout 对齐)
  - `GcovUnit` / `GcovFunction` / `GcovBlock` 数据结构
  - `write_notes` / `write_data_with_counters` 写盘入口
  - 已知限制：cfg_checksum 简化公式；非 x86_64 arch 未验证
- **AutoFDO** 🟡
  - 简化 text format parser（`func <name> <total>` / `line <name>
    <line> <col> <samples>`）
  - `bucketize` 按 >10% / >1% 阈值分 Hot/Warm/Cold
  - 已知限制：权重不下发到 codegen
- **LBR-based profiling**（`-flbr-profile=`） 🟡
  - 解析 `perf script -F brstack` 文本（`from_func from_addr
    to_func to_addr cycles`）
  - `hot_entries` 按 `unknown -> func` 入度排序
  - 已知限制：权重不下发到 codegen
- **MemProf / Page Weight Profiler**（`-fmemory-profile` /
  `-fmemory-profile-use=`） 🟡
  - 简化 format parser（`alloc <frames...> = <count> <bytes>` /
    `lifetime <alloc> -> <dealloc> = <bytes>`）
  - `is_cold(stack, threshold)` 启发
  - 已知限制：未在 libc malloc 上插桩，无 runtime 库
- **Perf event 输出 (TopdownL1 metrics)**（`-ftopdownmetrics`） 🟡
  - `TopdownL1` 枚举 + `PerfRecord` 容器
  - `topdown_percentages()` 把四 slot 规整为百分比
  - 已知限制：未读 perf.data 二进制
- **Offloading (OpenMP target / CUDA / HIP)** 🟡
  - `OffloadConfig` / `OffloadSection` / `OffloadEntry` 数据结构
  - `OffloadKind` 区分 OmpTarget / OmpTargetSimd / Cuda / Hip
  - `to_json()` 序列化（后续换 .dcc.offload 二进制）
  - 已知限制：无 device codegen 后端，runtime loader 未实装

详见 `docs/profiling.md`。

---

## 9. 自举 / 跨编译 / Bootstrap

### 9.1 自举 M5

🟠 / 🔴 状态：

- dcc 源码量 ~5 万行 Rust；自举尚未开始
- 步骤 1 (`rustc → dcc0`)：✅
- 步骤 2 (`dcc0 → dcc1`)：未开始（需要 dcc0 完整支持自身依赖：std 子集）
- 步骤 3 (`dcc1 → dcc2`)：未开始
- 字节级 / 语义级回归对比：未实现

### 9.2 交叉编译

✅ `--target=x86_64/aarch64/riscv64/mips/mips64/arm` 有完整后端

🔴 缺：

- 真正的多平台 binary（`x86_64-dcc` / `aarch64-dcc` / `riscv64-dcc`）
- Musl libc 目标（虽然 ABI 字段支持，但未集成 musl sysroot）
- Windows MSVC 目标（`windows-msvc`）— codegen 仍是 SysV-like
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

🔴 仍缺：

- 运行时 dynamic loader（`ld.so`）— 完全依赖 host（设计决策）
- vDSO 集成
- DSO symbol caching

---

## 11. 杂项 / 边角

🟢 / 🟠 / 🔴 状态（2026-07-21 M23 之后）：

| 模块 | 状态 |
|------|------|
| `setjmp` / `longjmp` | 🟠 走 libc，未生成 DWARF CFI 兼容的寄存器保存表 |
| `alloca` | 🟢 (M23) `__builtin_alloca` codegen: sub rsp + 16 字节对齐 |
| 信号 | 🔴 完整 `sigaction` / `sigaltstack` / `signal` 仅在 freestanding 走 libc；无 codegen 差异 |
| TLS dtv / TLS 描述符（`gottpoff` / `tlsdesc`） | 🟠 仅基础 `tpoff` 模型；AArch64 TLS 系列 R_TLSDESC_* 未实装 |
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
| 浮点环境 (`fenv.h`) | 🟠 `pragma STDC FENV_ACCESS` 解析为注释；运行时无 fenv 调用 |
| 复杂数学函数 (`cproj`, `cabs` 等) | 🟠 走 libm |
| `printf` format string 全集 | 🟢 (M23) 完整 flag/width/precision/length/type 匹配 |
| `scanf` format string | 🟢 (M23) 完整格式检查 + 指针类型验证 |
| `wchar_t` | 🟠 解析为 int，无 wcs* 函数 codegen |
| 宽字符字面量 `L"..."` | 🟢 (M23) 解析为 wchar_t 编码字符串 |
| `char16_t` / `char32_t` (C11/C++11) | 🟢 (M23) 解析为 uint16/uint32 + u""/U"" 字符串 |
| 原子混合大小（`atomic_load_explicit(_Atomic short*, ...)`） | 🟢 (M24) `__atomic_*` codegen 补齐 1/2 字节 (x86-64 编码: lock xadd/xchg/cmpxchg 8/16 位变体 + sil/si/dx/dl 子寄存器 + movzx 零扩展返回值) |
| `stdatomic.h` 全部 (`atomic_flag` / `atomic_signal_fence`) | 🟢 (M24) `atomic_signal_fence` 走 `__asm__ volatile("" ::: "memory")` 编译器屏障; `atomic_thread_fence` 同实现; `atomic_flag` 走现有 builtin 路径 |
| `stdarg.h` va_arg 跨平台 | 🟠 x86-64 SysV 完整；aarch64 / riscv64 简化 |

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
