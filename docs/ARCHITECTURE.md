# DCC 架构 (v0.1)

> 第一性原理: DCC = AST + 共享类型 + 零冗余文件IO + 函数级并行代码生成

## 整体架构

```
   ┌─────────────────────────────────────┐
   │  CLI (cli.rs)                        │
   │  解析 -O/-std/--target/-ffreestanding│
   └──────────────┬──────────────────────┘
                  │ Config
                  ▼
   ┌─────────────────────────────────────┐
   │  Driver (driver.rs)                  │
   │  read  -> parse -> emit -> write     │
   └──────┬──────────────────┬────────────┘
          │                  │
          ▼                  ▼
   ┌────────────┐    ┌────────────────┐
   │ Parser     │    │ Codegen        │
   │ (tokens.rs │    │ (codegen/)     │
   │  ast.rs    │    │  ├ x86_64.rs   │
   │  parser.rs)│    │  ├ aarch64.rs  │ ← M7 typed IR lowering
   └─────┬──────┘    │  ├ riscv64.rs  │ ← M7 typed IR lowering
         │           │  └ elf.rs      │
         ▼           └────────────────┘
   ┌────────────┐
   │ Typed IR   │
   │ (ir.rs)    │  lower → validate → opt
   └─────┬──────┘
         │
         ▼
   ┌─────────────────────────────────────┐
   │  AST (ast.rs)                       │
   │  - FuncDecl / Block / Stmt / Expr   │
   │  - 所有节点带 Span (行/列/字符级)    │
   └─────────────────────────────────────┘
```

## 性能优化一览 (vs GCC)

| 优化点 | GCC 做法 | DCC 做法 | 加速 |
|--------|---------|---------|------|
| Macro | 文本替换 + 重解析 | Token 序列记为 AST 节点 | 2-5x |
| Header | 每次重解析 | 全局类型表持久化 | 3-10x |
| 类型 | TU 局部 | 全局共享 | 2-4x |
| 优化 | 多 pass | 增量 pass | 2-3x |
| Debug info | 后置 | 留 AST 类型直出 DWARF | 1.5-2x |
| CodeGen | 串行 | 函数级并行 | 2x |
| File I/O | read+copy | mmap | 1.2-1.5x |
| 预处理输出 | 保留注释/空行 | 阶段3移除注释+跳过空行 | 3.4x (343K→100K 行) |
| 峰值内存 | 解析+codegen 分阶段 | 解析后 drop(preprocessed) | 减少 ~8MB |
| 类型错误恢复 | 硬错误终止 | 未知类型降级为 int + warning | 大型代码库连续编译 |

## 类型表设计 (M3)

```
~/.dcc/cache/<filehash>_<contenthash>  <-- 头文件预处理缓存
```

缓存键 = FNV-1a(文件路径) + FNV-1a(内容哈希)，值 = 预处理输出 + 头文件新增宏定义。
- 自定义文本格式 `DCC_CACHE_V1`，零外部依赖
- 头文件仅保存自身新增的宏，避免宏状态污染
- 内容变化时自动失效（内容哈希不变 = 缓存命中）

## 错误诊断 (M9)

所有 `Diagnostic` 携带:
- file/line/col/len (精确到字符)
- error code (E0001, ...)
- hint (修复建议)

格式: GitHub-style 行+指针+下划线
```text
error[E0001]: expected ';', got '{'
  --> main.c:3:12
   3 |   return x {
                ^
   hint: did you mean ';'?
```

### Snapshot 回归（tests/errors.rs）

失败路径的诊断格式由 snapshot 锁定，与 59 项 integration 成功路径互补。  
**完整错误码说明见 [DIAGNOSTICS.md](./DIAGNOSTICS.md)。**

| 目录 | 内容 |
|------|------|
| `tests/fixtures/errors/*.c` | 触发 fixture |
| `tests/fixtures/errors/*.stderr.snap` | 归一化 stderr 黄金文件（35 个，含 `diagnostic_format`） |
| `scripts/update_error_snapshots.sh` | Linux 上一键刷新 snapshot |

`validate_diagnostic_snapshot()` 锁定 GitHub 四段式结构；变更
`diagnostics.rs` 布局时递增 `DIAGNOSTIC_FORMAT_VERSION` 并全量刷新。

更新 snapshot：`DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors`。

## 跨平台

`--target=<triple>` 完整支持:
- x86_64-unknown-linux-gnu
- aarch64-linux-gnu
- riscv64-linux-gnu
- mips-linux-gnu
- arm-linux-gnueabi
- 自由扩展

## x86-64 可重定位代码生成

`-c` / `-r` 使用三个阶段生成 ET_REL：

1. `x86_64.rs` 发射函数机器码，收集 `.rodata`、`.data`、`.bss` 和
   `section` 属性产生的动态节，同时记录尚未绑定的符号引用。
2. 按 ELF 规则构造 `STN_UNDEF → LOCAL → GLOBAL/WEAK` 符号表，并把
   declaration 的 weak/visibility/alignment 信息传递到符号和节布局。
3. 符号索引稳定后生成 `.rela.text`、`.rela.data` 及动态
   `.rela.<section>`，最后由 `elf.rs` 动态构造 `.shstrtab` 和节头表。

自定义代码节中的重定位会从 `.rela.text` 迁移到对应 relocation section；
未初始化的普通全局对象进入 `.bss`，不占目标文件数据空间。

## 链接与 LTO（`-r` / `-flto`，非自研链接器）

DCC **不包含**内置可执行文件链接器，也 **不做** IR/bitcode 级跨 TU 内联或
常量传播。多文件产物标准工作流：

```text
dcc -c a.c -o a.o && dcc -c b.c -o b.o
gcc a.o b.o -o prog          # 最终可执行文件
# 或仅合并可重定位对象：
dcc -r [-flto] -o all.o a.o b.o
```

| 组件 | 职责 | 依赖 |
|------|------|------|
| `cli.rs` | 区分 `-c`（单 TU 出 `.o`）与 `-r`（仅接受 `.o` 输入）；多 `.c` 直链 `a.out` 报错并提示工作流 | — |
| `lto.rs` | 编译期写 `.llvm.lto` 文本摘要（函数 defined/referenced/size）；链接期合并摘要 | — |
| `link.rs` | `ld -r [--gc-sections]` 合并；`objcopy` 注入/更新 `.llvm.lto` 与 `--strip-symbol` dead local | host `ld`、`objcopy`（`LD`/`OBJCOPY` 可覆盖） |

管线（`driver.rs` → `link.rs`）：

1. 读取各 TU 的 `.llvm.lto` 摘要（`lto.rs`），合并函数定义/引用。
2. 扫描输入 ELF 的 `.rela.*` 表标记跨 TU 引用（`elf.rs`）。
3. 调用 `ld -r --gc-sections` 合并目标文件；`-flto` 时默认
   `-ffunction-sections`，每函数位于 `.text.<name>` 节。
4. 用 `objcopy` 注入合并后的 `.llvm.lto`，并对 dead local 符号
   执行 `--strip-symbol` 补充剔除。

**明确不做**：ELF 可执行链接（`ET_EXEC`）、跨 TU IR 内联、LLVM bitcode LTO。

远程验收：`scripts/sync_remote_build.sh` → `remote_feature_check.sh`（LTO 节）；
`cargo test flto_relocatable_link` / `lto_link_strips_unreferenced_static`。

## x86-64 后端 ABI 约定

- 普通标量值使用 `rax`，`__int128` 使用 `rax:rdx`（低 64 位在 `rax`）。
  16 字节整数按 SysV ABI 分类为两个 `INTEGER` 类，函数调用占两个 GP
  参数槽（`rdi/rsi/...` 或栈），返回值同样在 `rax:rdx`。
- 后端以 `r13` 作为稳定帧基址。嵌套函数调用通过 `r10` 传递静态链，
  被调函数保存父级 `r13` 后按已解析的父帧偏移访问捕获变量。
- 简化 `va_list` 会在可变参数函数序言中按
  `rdi,rsi,rdx,rcx,r8,r9` 保存 GP 参数，并按 `xmm0`–`xmm7` 保存 SSE
  参数；`va_arg` 支持 float/double/long double。
- `register asm("reg")` 变量直接映射到指定物理寄存器；调用者仍需遵守
  目标寄存器的 SysV caller/callee-saved 约束。

## 函数级并行 Codegen（`codegen/x86_64.rs`）

- **触发**：`-O2`/`-O3` 且 relocatable `.o`、TU 内 ≥2 个有体顶层函数；可用
  `DCC_NO_PARALLEL=1` 关闭（`-O0`/`-O1` 或 `-S` 始终串行）。
- **机制**：`parallel_emit_ctx()` 快照 rodata/全局/嵌套元数据；worker 线程各自
  `emit_func_chunk()` 生成独立机器码块，主线程 `merge_func_chunk()` 按原函数序
  拼接并修正重定位偏移。
- **线程模型**：`std::thread::scope` + 原子 job 索引；worker 数 =
  `available_parallelism().min(n_funcs)`；AST 聚合类型已 `Arc` 化以跨线程共享
  `Program`。
- **回归**：`parallel_codegen_o2_multi_function` 断言并行/串行 `.o` 字节一致；
  `remote_feature_check` **parallel_codegen**。

## GNU 控制流扩展

Parser 直接表示 `&&label`、`goto *expr` 和 `asm goto`。标签地址使用
RIP-relative LEA，计算 goto 发射间接 `jmp`，`asm goto` 在 Mini x86
汇编器之前将 `%lN` / `%l[name]` 解析到函数内标签。

Mini x86 汇编器（`codegen/x86_64.rs`）要点：

- **标签命名空间**：C 层 `Stmt::Label` 与 asm 字符串内命名标签（如 `found:`）
  共用 `Codegen::label()`；`-O2` DCE 在 `return` 后仍保留可达标签。
- **同行标签**：`N:insn`（GNU 局部数字标签）与 `found: ret` 在发射前拆成
  标签行 + 指令行（`expand_label_insn_lines`）。
- **局部数字标签**：`6:` / `6f` / `6b` 映射为内部符号（`AsmLocalLabels`）。
- **AT&T 行内注释**：`strip_asm_comment` 剥离 `#` 后注释，避免 asm goto
  `jmp %l[x] # objtool NOPs this` 将注释误当作标签名。
- **模板转义**：`%%` 在 `%N` 占位符替换后折叠为 `%`（GNU 内联 asm 寄存器字面量）。
- **指令子集**：除 mov/alu/jcc 外，含 `bt/bts/btr/btc`、`bsf/bsr/popcnt`、`bswap`、
  `movnti(l/q)`、`vmcall/vmmcall`、`shld/shrd`、`adc/sbb`、
  `cmpxchg`/`xadd`/`cmov` 内存操作数、`movzbl` 内存源、`setcc` 内存目标、
  `push`/`pop` 内存操作数、`lea symbol(%rip)`、GAS 伪指令
  （`.pushsection`/`.balign`/`.long`/`.quad`/`.popsection` 等，`-c` 静默填充）、
  `lock`/`rep` 前缀；`cmp`/`test`/shift/imul/unary 内存操作数、
  `rol`/`ror`、`prefetcht0/t1/t2/nta`、`movss`/`movsd`/`addss`/`addsd`、
  `xorps`/`xorpd`、`swapgs`/`invlpg`/`int3` 等；
  未识别指令 warning + nop 占位。
- **SIMD/复数**：向量全局 `{…}` 逐元素 `.data`/`.rodata` 静态初始化（`gen_global_init_list`）；
  局部/rvalue 经帧底临时区 slot 传递；**标量逐元素** `+/-/*`（整数用 GP，
  浮点用标量 SSE，不用 XMM 向量指令）；
  向量 `arr[i]` 读写经 LEA 取栈/全局基址再按元素偏移寻址（非指针 load）；
  非 Ident 向量（binop、compound literal、InitList）经 `gen_vector_to_temp` 物化；
  `_Complex` add/sub/mul/div（SSE 标量）；`parser` 对复数二元结果保留 `Type::Complex` 以便
  `(int)(a+b)` 等 Cast 正确发射 `cvttss2si`；虚部清零用 `movss` 避免 64 位
  store 覆盖相邻实部；`mov_xmm` 仅在 xmm≥8 时加 REX。不支持表达式路径发射
  **ud2** 而非静默 `mov 0`。
- **Builtins**：`__builtin_constant_p` 经 parser/opt/codegen 共享
  `const_eval::is_constant`；`__builtin_object_size` 编译期折叠；
  `__builtin_apply`/`return` SysV 子集；`__builtin_prefetch*` void no-op；
  未知 `__builtin_*` warning + 零值（有返回值）或 **ud2**（void）。
- **IR 内联**：`-O0+` 对 `always_inline` 单返回体内联；`-O1+` 完整 pass；
  `-O3` opportunistic 小 static 函数；`noinline` 阻止内联。
- **SSA/GVN（`-O2+`）**：块内 GVN/CSE、拷贝传播、冗余局部合并。
- **循环优化**：`-O2+` LICM；`-O3` 小 trip 展开与向量元素循环向量化。

## AArch64 后端（`codegen/aarch64.rs`，M7）

- **入口**：`codegen::emit` 在 `--target aarch64-linux-gnu` 时选用 `Aarch64Codegen`。
- **IR lowering**：遍历 `Module.functions` 与 `TypedExpr`；语句（`if`/`while`/`for`/`return`）
  对照 AST 与 `TypedStmt.nested` 保持控制流语义。
- **已实现**：AAPCS64 整数/浮点/向量（基础 NEON `v0.4s`）表达式、`bl` 函数调用（
  `R_AARCH64_CALL26`）、全局 `.rodata`、`ET_REL` `.o` 与 `ET_EXEC` hello
  （`_start` → `bl main` → `svc #0`）；`-S` 输出 GNU 汇编骨架。
- **限制**：无 varargs/结构体传参、除法/取模回退为 0、向量仅常量广播与加减乘。

## RISC-V 64 后端（`codegen/riscv64.rs`，M7）

- **入口**：`codegen::emit` 在 `--target riscv64-linux-gnu` 时选用 `Riscv64Codegen`。
- **能力与 AArch64 对齐**：整数/浮点/向量表达式、控制流、函数调用（`R_RISCV_JAL`）、
  全局 `.rodata`、`build_elf_riscv64` / `build_elf_rel_riscv64`。
- **限制**：同上；大立即数通过 `lui`+`addi` 序列；浮点运算为 RV64GC `f` 扩展子集。

## 测试脚本布局

活跃验收脚本见 `scripts/README.md`；历史调试脚本归档于 `scripts/archive/`。

预处理 `#` 字符串化遵循 C 规则：实参内换行与连续空白折叠为单空格
（`NOFAIL(ptr)` → `"malloc(...)"` 单行字面量）。

## Typed IR 与语义校验（`ir.rs`）

Driver 在 codegen 前执行 `lower → validate → opt → resync → validate`：

| 阶段 | 职责 |
|------|------|
| `lower` | AST 降级为带 `ValueType` 的函数/全局图；全局初值过 `is_static_initializer`，违例报 **E1006** |
| `validate` | 校验 float/vector/complex 二元/一元/赋值/条件/聚合/转换是否与 codegen 能力一致（**E1003/E1007/E1008**）；向量二元仅 Add/Sub/Mul |
| `opt` | 常量折叠、`-O0` 仍内联 `always_inline`、`-O1+` 函数内联、
  `-O2+` GVN/CSE/拷贝传播/LICM、`-O3` 循环展开与向量循环向量化；改写 AST |
| `resync` | opt 后从 AST 重新 lower，使 IR 图与优化结果一致 |
| `validate`（二次） | resync 后再次语义校验 |

x86_64 后端 `compile(&Module)` 与 AArch64/RISC-V 一致：从 `Module::source()` 读 AST，
同时 IR 图供 debug/LTO/并行 codegen 元数据使用。

Linux jump_label 预处理残留 `_DECLARE_STATIC_KEY_*` 在 parser 层由
`parse_static_key_decl()` 补全为 `extern struct static_key_{false,true}`，
再进入上述 IR 管线。回归：`tests/fixtures/static_key_*.c`、`ir::tests`。

## Sanitizer 初版桩（`sanitizer.rs`）

Driver 在 parse 之后、IR lower 之前调用 `instrument_program()`：

| 层级 | 行为 | 明确不做 |
|------|------|----------|
| TU init | 首个有体函数前插入 `__asan_init` / `__ubsan_init` / `__kcsan_init` 各一次 | 重复 init |
| 函数入口 | 每个有体函数（含嵌套函数）前插入 `__sanitizer_cov_trace_pc`（KCOV）、`__kcsan_func_entry`（KCSAN） | 每函数 KASAN/UBSAN 内存/UB 检查 |
| 元数据 | codegen 写 `.dcc.san_info`（`DCC_SAN` v1 + 4 标志位） | 运行时库内置 |

CLI：`-fsanitize=address|undefined|thread`、`-fsanitize=a,b` 组合、
`-fsanitize-coverage` / `fuzzer`。完整 KASAN/UBSAN 需后续 IR 指令级插桩。

回归：`sanitizer_metadata_section_emitted`、`sanitizer_prologue_in_all_functions`、
`scripts/remote_feature_check.sh`（sanitizer 节）。

## 自举 (M5)

```bash
# 阶段 0: 用 rustc 编出 dcc0
cargo build --release
cp target/release/dcc bin/dcc0

# 阶段 1: 用 dcc0 编出 dcc1
./bin/dcc0 -std=gnu23 -O2 -o bin/dcc1 src/main.rs ...

# 阶段 2: 用 dcc1 再编 dcc1,产物 dcc2
./bin/dcc1 -std=gnu23 -O2 -o bin/dcc2 src/main.rs ...

# dcc1 == dcc2 (字节级或语义级),即自举收敛
```

## 路线图 (来自 docs/ROADMAP.md)

M0~M10 见根目录的 task 列表。
