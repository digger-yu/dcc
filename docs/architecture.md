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

缓存键 = FNV-1a(文件路径) + FNV-1a(内容哈希) + FNV-1a(宏表状态),
值 = 预处理输出 + 头文件新增宏定义。
- 自定义文本格式 `DCC_PREPROCESSOR_CACHE_V7` (M19 升级), 零外部依赖
- 头文件仅保存自身新增的宏, 避免宏状态污染
- 内容变化时自动失效 (内容哈希不变 = 缓存命中)
- 宏表变化时自动失效 (例如 `-D_FORTIFY_SOURCE=2` vs `=0`),
  因为 `hash_macro_state` 把按名排序的 name+body+params+variadic
  都纳入缓存键
- M19 后 `macros.is_empty()` 不再触发 `expand_macros` 早退,
  保证 `__COUNTER__` / `__FILE__` / `__LINE__` 等特殊 token
  始终进入展开路径

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
**完整错误码说明见 [diagnostics.md](./diagnostics.md)。**

| 目录 | 内容 |
|------|------|
| `tests/fixtures/errors/*.c` | 触发 fixture |
| `tests/fixtures/errors/*.stderr.snap` | 归一化 stderr 黄金文件（35 个，含 `diagnostic_format`） |
| `scripts/update_error_snapshots.sh` | Linux 上一键刷新 snapshot |

`validate_diagnostic_snapshot()` 锁定 GitHub 四段式结构；变更
`diagnostics.rs` 布局时递增 `DIAGNOSTIC_FORMAT_VERSION` 并全量刷新。

更新 snapshot：`DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors`。

## 跨平台

`--target=<triple>` 分两类:

| 类别 | Triple | 含义 |
|------|--------|------|
| 完整后端 (has_backend == true) | `x86_64-unknown-linux-gnu` / `aarch64-linux-gnu` / `riscv64-linux-gnu` / `mips-linux-gnu` / `mips64-linux-gnuabi64` / `arm-linux-gnueabihf` | `codegen::emit` 真正产出机器码,ET_EXEC/ET_REL 正常写出 |

设计动机: Linux kernel 编译链常在 host 端用 `--target=...` 探测远端架构,
正确解析几何参数确保上层预处理、宏展开、IR lowering 等阶段可以无差别地基于 target 做选择。

第一性原理:
- `Arch::has_backend()` (src/cli.rs) 是单一真相点,所有调用者通过它判定
  是否能 emit。
- `Target::resolve()` 不管后端实现 — 它只负责 "几何" (指针/字节序/调用约定),
  所有架构都正确填字段,这样上层预处理、宏展开、IR
  lowering 等阶段可以无差别地基于 target 做选择,无需 `match arch`。
- `codegen::emit()` 的兜底分支直接 panic,文本来自 `backend_missing_msg()`。

架构几何:

| 架构 | 指针 | long | 字节序 | 调用约定 | ELF 类 |
|------|------|------|--------|----------|--------|
| x86_64 | 8 | 8 | Little | SysV | ELF64 |
| AArch64 | 8 | 8 | Little | AAPCS64 | ELF64 |
| RISC-V 64 | 8 | 8 | Little | SysV | ELF64 |
| MIPS | 4 | 4 | Big | O32 (AAPCS) | ELF32 |
| MIPS64 | 8 | 8 | Big | N64 (AAPCS) | ELF64 |
| ARM | 4 | 4 | Little | AAPCS | ELF32 |

## x86-64 可重定位代码生成

`-c` / `-r` 使用三个阶段生成 ET_REL：

1. `x86_64.rs` 发射函数机器码，收集 `.rodata`、`.data`、`.bss`、
   `.tdata`/`.tbss`（`_Thread_local`）和 `section` 属性产生的动态节，
   同时记录尚未绑定的符号引用。
2. 按 ELF 规则构造 `STN_UNDEF → LOCAL → GLOBAL/WEAK` 符号表（TLS 用
   `STT_TLS`），并把 declaration 的 weak/visibility/alignment 信息传递到
   符号和节布局。
3. 符号索引稳定后生成 `.rela.text`、`.rela.data` 及动态
   `.rela.<section>`，最后由 `elf.rs` 动态构造 `.shstrtab` 和节头表。

ET_EXEC（无 `-c`）由 `build_elf_exec_x86_64` 附加 `.symtab`/`.strtab`；
TLS 变量经 `_start` 中 `arch_prctl(ARCH_SET_FS)` 绑定 FS 段。

Driver 在 parse 后运行 `format_check::check_program`（`-Wformat` / `W0003`）。
未知 typedef 默认 **E0009**；`-fpermissive` 降级为 `int` + **W0002**。
hosted 模式（非 `-ffreestanding`）预置 C11 `atomic_*` 宏。

自定义代码节中的重定位会从 `.rela.text` 迁移到对应 relocation section；
未初始化的普通全局对象进入 `.bss`，不占目标文件数据空间。

## 链接与 LTO（`-r` / `-flto` / `-pie` / `-shared` / `-fbuiltin-linker`）

DCC 现已内置完整的 x86-64 链接器（`src/link/`），对标 mold/lld 行为：

- **单 TU 路径**（`dcc a.c` 直出可执行文件）：单 TU 使用 `R_X86_64_PLT32`
  重定位，仅 stub PLT（不写 `.dynamic`、不生成完整链接器代码路径）。
- **多 TU 路径**（`dcc -fbuiltin-linker a.o b.o -o prog`）：启用完整
  `src/link` 链接器：ELF ET_REL 解析、跨 TU 符号解析、节合并、虚拟地址
  布局、完整重定位引擎、PLT/GOT 生成、ET_EXEC/ET_DYN 输出。
- **`-r` 模式**（合并可重定位对象）：通过 host `ld -r --gc-sections` 合并，
  `objcopy` 注入 `.llvm.lto` 摘要，并对 dead local 符号执行 `--strip-symbol`
  补充剔除。兼容旧工作流。
- **`-pie` / `-shared`**：输出 ET_DYN（PIE 或 `.so`），生成 `.dynamic`、
  `.dynsym`、`.dynstr`、`.rela.dyn`、`.rela.plt`、`.plt`、`.got`、`.got.plt`。
- **`-flto` IR/bitcode LTO**：每 TU 末尾在 `.dcc.lto` 节写入自定义位码
  （魔数 `DBC` + 版本 1），链接时反序列化、做跨 TU dead-code 消除。

| 组件 | 职责 | 依赖 |
|------|------|------|
| `cli.rs` | 区分 `-c` / `-r` / `-pie` / `-shared` / `-fbuiltin-linker` / `-Wl,-soname=…` | — |
| `link/object.rs` | 解析 ELF ET_REL 目标文件：节、符号、`.rela.*` 重定位 | — |
| `link/resolve.rs` | 跨 TU 符号解析：强 > 弱 > undef；隐式 undef 允许 libc weak | — |
| `link/layout.rs` | 同名节合并 + 2 段布局（`R+X` / `R+W`） + 自定义节独立 | — |
| `link/relocate.rs` | 重定位引擎：`PC32` / `PLT32` / `64` / `32` / `32S` / `GOTPCREL` / `GOT64` / `GOTOFF64` / `TPOFF64` / `TPOFF32` / AArch64（22 种）/ RISC-V（15 种） | — |
| `link/plt_got.rs` | PLT/GOT 生成、`.dynamic`、`.dynsym`、`.rela.{dyn,plt}` | — |
| `link/elf_exec.rs` | ET_EXEC 输出：ELF 头、PHDR、节头、`.symtab`、`.strtab`、`.gnu.hash` | — |
| `link/elf_dyn.rs` | ET_DYN 输出：PIE / `.so` 共享库 | — |
| `link/gnu_hash.rs` | `.gnu.hash` TIE 压缩（bloom filter + buckets） | — |
| `link/eh_frame.rs` | `.eh_frame_hdr` 二分搜索表 | — |
| `link/build_id.rs` | `.note.gnu.build-id` 段（SHA-1 / MD5 / UUID v4） | — |
| `link/ifunc.rs` | IFUNC（STT_GNU_IFUNC）解析 + `R_X86_64_IRELATIVE` 生成 | — |
| `link/lto_bitcode.rs` | 自有 DBC 格式 IR 位码序列化 + 跨 TU DCE / 内联 / 常量传播 / DAE | — |
| `link/legacy.rs` | 旧 `-r` 模式（host `ld -r`） | host `ld`、`objcopy` |
| `link/link_opts.rs` | `-z relro/now/lazy/execstack/noexecstack/pack-relative-relocs/max-page-size=N`、RPATH/RUNPATH、-fuse-ld=、-fno-semantic-interposition、SHF_GNU_RETAIN | — |
| `link/symver.rs` | `.symver` 完整版本表（VERDEF / VERNEED） + `--no-undefined-version` 严格检查 | — |
| `link/target_clones.rs` | `__attribute__((target_clones(...)))` IFUNC resolver 多版本生成 | — |
| `link/fini_dispatch.rs` | `__dso_handle` / `__cxa_atexit` / `atexit` / `.fini_array` 调度 | — |

**多 TU 链接管线**（`driver.rs::run_builtin_linker`）：

1. `parse_object` 读取每个 `.o` 的节 / 符号 / 重定位（`object.rs`）。
2. `resolve` 做跨 TU 强/弱/undef 规则合并（`resolve.rs`）。
3. `LayoutBuilder` 按同名节合并并分配虚拟地址（`layout.rs`）。
4. 动态构建 PLT/GOT 与 `.dynamic` 节（`plt_got.rs`，仅 `-pie`/`-shared`）。
5. `apply_relocations` 写入最终字节；动态重定向分离到 `.rela.dyn` / `.rela.plt`。
6. `write_elf_*_x86_64` 写出 ET_EXEC / ET_DYN / ET_REL。

**IR/Bitcode LTO 管线**（`lto_bitcode.rs`）：

1. Codegen 在 `.dcc.lto` 节写入每个 TU 的 IR 摘要（`DBC\0` 头 + version 1）。
2. `serialize_module` 把 `ir::Module` 编码进 DBC 流（含所有 `ValueType`、
   `BinOp` / `UnOp` / `ExprKind` / 函数 / 全局）。
3. `deserialize_module` 反向重建模块。
4. `run_cross_tu_lto` 跨 TU 优化（4 pass）：
   - 跨 TU 常量传播：合并 `global constant` 初始化值，替换跨 TU 引用。
   - DCE：标记未引用函数 / 全局为 dead。
   - 跨函数内联：cost model（≤4 语句 / 4 表达式 / 无递归）的小函数
     （含同 TU 与跨 TU 调用）原地展开。
   - 全局 DAE：消除 `global` 中无用的 store 副作用。
5. `LinkConfig` CLI 入口支持 `-Wl,--exclude-libs=libxxx.a`（被排除的归档
   库中的全局符号打 `STV_HIDDEN`），`--build-id=sha1|uuid|md5|none`
   （生成 `.note.gnu.build-id` 段），`--no-undefined-version`（拒绝
   `.symver` 缺版本的符号引用，通过 `link/symver.rs` 完整版本表校验）。

**M12+ GNU ld 兼容链接特性**（`link/link_opts.rs` + `link/symver.rs` +
`link/target_clones.rs` + `link/fini_dispatch.rs`）：

- `-z relro` / `-z relro=partial|full`：写 `PT_GNU_RELRO` 段，覆盖范围
  由 `compute_relro_range` 计算（包含 `.got` / `.got.plt` / `.init_array`
  / `.fini_array` / `.data.rel.ro`）。
- `-z now` / `BIND_NOW`：在 `.dynamic` 写 `DT_FLAGS = DF_BIND_NOW`
  (0x8) 和 `DT_FLAGS_1 = DF_1_NOW` (0x1)，关闭延迟绑定。
- `-z lazy`：恢复 `DF_LAZY`（默认）。
- `-z execstack` / `-z noexecstack`：控制 `PT_GNU_STACK` 的 `PF_X` 标志。
- `-z max-page-size=N` / `-z common-page-size=N`：解析 K/M/G 后缀和
  `0x` 前缀，控制 `PT_LOAD` 的 align 字段。
- `-z pack-relative-relocs`：生成 `.gnu.pack.relocs` 段（SHT_GNU_RELR，
  内核/RTOS 友好的 RELR 压缩）。
- `-rpath <path>` / `-runpath <path>`：写 `DT_RPATH` (23, deprecated) /
  `DT_RUNPATH` (29, glibc 推荐) 动态项。
- `-fuse-ld=<name>`：选择链接器后端（`dcc` 内置 / `gold` / `lld` /
  `bfd`），`DCC_FUSE_LD` 环境变量可覆盖。
- `-fno-semantic-interposition` / `-fsemantic-interposition`：LTO 阶段
  控制函数符号插入语义（`LinkConfig.no_semantic_interposition`）。
- `--gdb-index`：生成 `.gdb_index` 节（最小占位：版本字符串 +
  0 填充，GDB 可识别但内容不完整）。
- `--compress-debug-sections=zlib|zstd`：占位，当前忽略。
- `--lto-partition=N`：接受但忽略（单分区足够）。
- `SHF_GNU_RETAIN` 标志（0x200000）：对 `.init` / `.fini` /
  `.init_array` / `.fini_array` / `.text.startup` 等元数据节自动标记，
  `gc-sections` 即使无显式引用也保留。
- `gc_sections_root` 自动选根：除 `entry` 外接受 `-u <sym>` /
  `-Wl,-u,<sym>` 显式根符号，根集合从 `entry` + `-u` + 用户指定符号
  并集 + `.init`/`.fini`/`.init_array`/`.fini_array` 等始终 live 节
  出发做引用图 BFS，软删除未被引用的节（`output_index = u32::MAX`，
  `size = 0`）。
- `.symver` 完整实现（`link/symver.rs::VersionTable`）：
  - `add_symver(".symver memcpy,memcpy@GLIBC_2.14")` 解析
    `name,alias@ver` 形式，区分 `@ver` 和 `@@ver`（hidden）。
  - `gen_gnu_version(dynsym_names)` 生成 `.gnu.version` 节（每个
    dynsym 一个 16-bit 索引，0=VER_NDX_LOCAL, 1=VER_NDX_GLOBAL, 2+=
    版本化）。
  - `gen_verdef()` 生成 `.gnu.version_d`（VERDEF 结构 + 字符串表）。
  - `check_undef(refs)` 在 `--no-undefined-version` 模式下严格检查。
- IFUNC `target_clones`（`link/target_clones.rs::TargetClonesTable`）：
  - 解析 `__attribute__((target_clones("sse4.2,default")))` 字符串。
  - 生成 STT_GNU_IFUNC 类型符号 + `R_X86_64_IRELATIVE` 重定位。
  - Resolver 简化实现：直接返回 default 变体地址（无 CPU feature 探测）。
- `__dso_handle` / `.fini_array` 调度（`link/fini_dispatch.rs`）：
  - `__dso_handle` 指向 `.data` 起始。
  - `FiniArrayBuilder` 收集 destructor 符号，按 priority 排序生成
    `.fini_array` 数据。
  - `ensure_fini_array_section` 在 layout 中添加缺失的 `.fini_array` 节。
- `-r` 自研路径：新增 `OutputKind::Relocatable` + `write_elf_relocatable`
  生成 ET_REL .o（简化：复用 ET_EXEC 写盘路径，改 e_type；完整
  ET_REL 输出需独立写节头/重定位，见 TODO）。

**重定位类型**（`relocate.rs`）：

- x86-64：`R_X86_64_PC32` / `PLT32` / `64` / `32` / `32S` / `GOTPCREL`
  / `GOT64` / `GOTOFF64` / `TPOFF64` / `TPOFF32` / `IRELATIVE` / `DTPMOD64`
  / `DTPOFF64`。
- AArch64：`ABS64` / `PREL64` / `CALL26` / `JUMP26` / `ADR_PREL_LO21`
  / `ADR_PREL_PG_HI21` / `ADD_ABS_LO12_NC` / `LDST*_ABS_LO12_NC`
  / `MOVW_UABS_G0..G3`。
- RISC-V：`64` / `32` / `32_PCREL` / `CALL` / `CALL_PLT` / `PCREL_HI20`
  / `PCREL_LO12_I` / `PCREL_LO12_S` / `HI20` / `LO12_I` / `LO12_S`
  / `TPOFF64` / `TPREL_HI20` / `TPREL_LO12_I` / `TPREL_LO12_S`。

**明确不做**：AArch64 / RISC-V 完整后端 ELF 输出（沿用 host 链接器）
之外的非 x86-64 后端。

本地验收：`cargo test link::`（50 用例）
+ `cargo test lto::`（7 用例）。

## C++ 前端 M7（`src/cpp/`）

DCC 在 M7 阶段新增 C++ 子集前端，为 GCC bootstrap 做准备。设计原则：
**不另起 IR**，而是把 C++ AST 降级（lower）到 DCC 现有的 C 风格声明，
复用 C 前端 + 链接器整条链路。

| 文件 | 职责 |
|------|------|
| `src/cpp/lexer.rs` | C++ token 流：扩展关键字 + `::` 标点 + M8 关键字（operator/namespace/try/catch/throw/...） |
| `src/cpp/parser.rs` | class / ctor / dtor / 成员函数 / 成员变量 / `this->x` 解析 + M8 (namespace/operator/inherit/lambda/template/try-catch) + M10+ (partial spec/using Base::Base/init list/static local) + M17 (decltype/auto/friend/noexcept/final/override/mutable/volatile/explicit/using-in-class) + M18 (multi-dim subscript `obj[i,j,k]` → `CppExpr::MultiIndex`) |
| `src/cpp/mangle.rs` | Itanium C++ ABI 名字修饰（`_ZN…C1E…` / `D1E` / `pl` / `_ZTV…` / `_ZTI…`） |
| `src/cpp/lower.rs` | C++ AST → C 字符串（struct + 隐式 this 形参 + `_Znwm`/`_ZdlPv` 替身 + vtable/typeinfo/lambda/exception 降级 + M10+ cxa_guard/init_list/coroutine 状态机 + M18 多维下标改写为 `__dcc_multi_index_N`） |
| `src/cpp/modules.rs` | C++20 Modules 占位 (M18)：已知 module 表 + `import X;` 改写策略；M19+ 实现 BMF 序列化 |
| `src/cpp/stdlib/headers/*` | 26 个 C++ 标准库头（M8-M18）：cstddef/cstdlib/cstring/cstdio/string/vector/iostream/memory/exception/stdexcept/typeinfo/initializer_list/utility/algorithm/map/functional + M18 chrono/thread/mutex/condition_variable/filesystem/expected/flat_map/regex/ranges |
| `src/cpp/m10.rs` | M10+ 高级特性降级器：SFINAE / partial spec / concept 求值 / 协程 / module 分区 / operator-> / dynamic_cast / 构造委托 / 成员函数指针 / __cxa_guard_ / initializer_list |
| `src/cpp/driver.rs` | 顶层入口：`compile_cpp(cpp_src, obj, dcc_bin, includes, opt)` |

**降级映射表**（C++ → C）：
- `class Foo { int x; }` → `struct Foo { int x; };`
- `Foo::Foo(int v) { this->x = v; }` → `void _ZN3FooC1Ei(struct Foo* this, int v) { this->x = v; }`
- `Foo::~Foo()` → `void _ZN3FooD1Ev(struct Foo* this) { }`
- `T& ref` → `T* ref`（引用降级为指针）
- `new Foo(...)` → `((struct Foo*)_Znwm(sizeof(struct Foo))); _ZN3FooC1E…(...)`
- `delete p` → `_ZdlPv(p);`
- `namespace ns { class A {}; }` → 扁平化为 `struct ns_A {};`（命名空间前缀化）
- `class D : public B {}` → `struct D { struct B __base_B; ... };`（基类嵌入）
- `virtual void f() {}` → `void _ZN1DfEv(struct D* this);` + `_ZTV1D[]` vtable
- `operator+` → `_ZN...pl...`（Itanium 运算符编码：pl/mi/ml/dv/...）
- `try { throw 1; } catch (int e) {}` → `__cxa_try`/`__cxa_allocate_exception`/`__cxa_throw` 调用
- `[](int x) { return x; }` → 匿名 `struct __lambda_N` + `_ZN__lambda_NclERKS_` operator()
- `template <typename T> ...` → 语法占位 + 单态化降级
- `typeid(e)` / `dynamic_cast<T*>(e)` → RTTI 占位（`_ZTI` 符号 + `__vptr` 字段）

**M7 范围**：class、ctor、dtor、成员变量、成员函数、`this->x`、`::`、
引用（→指针）、new/delete、MangleType 基本类型。
**M8 范围**（新增）：
- 命名空间（`namespace foo {}` / `using namespace bar` / `::name`）
- 运算符重载（除 new/delete 外，含 `operator()` / `operator T`）
- 继承（单 / 多 / 钻石继承）+ 虚函数 + vtable（`_ZTV<Name>`）+ typeinfo（`_ZTI<Name>`）
- RTTI：`typeid(e)` / `dynamic_cast<T*>(e)` 解析 + `_ZTI` 符号
- 异常：`try { ... } catch (T& e) { ... }` / `throw e` → Itanium zero-cost 降级（`__cxa_*` 调用）
- Lambda：捕获列表 + 形参 + body → 匿名 `struct` + `operator()` 函数
- 模板：`template <typename T>` 语法 + 实例化（简化：单态化）
- constexpr / consteval：关键字识别（占位）
- `std::complex` / `std::atomic` 风格标准库组件：依赖模板 + 运算符

**M9 范围**（新增）：
- **CLI 集成**：`dcc -x c++ foo.cpp` / `dcc foo.cpp`（按 `.cpp`/`.cc`/`.cxx`
  扩展名自动识别）；`dcc -dump-ast` / `dcc -fsyntax-only` 模式
- **C++ 词法补全**：C 关键字（`int` / `return` / `if` / `while` / ...）在 C++
  源码里被识别为 `TokKind::KwInt` 等，不再被误作 `Ident`；全部复合操作符
  `->` / `->*` / `--` / `-=` / `++` / `+=` / `<<` / `<<=` / `>>` / `>>=`
  / `==` / `!=` / `<=` / `>=` / `&&` / `||` / `*=` / `/=` / `%=` / `^=`
  / `|=` / `&=` / `...` 完整支持
- **C++20 占位降级**：
  - `module foo;` / `export module foo;` / `import <iostream>;` /
    `import "header.h";` / `export { ... }` 降级为 no-op 注释
  - `concept Name = ...;` / `requires ...` 降级为占位注释
  - `co_await` / `co_yield` 降级为表达式 + 注释；`co_return` 降级为 `return`
- **C++ → C 降级完善**：
  - 局部变量声明 `T name;` / `T name = init;` / `T name(args);` 解析 + 降级
    为 `struct T name;` 形式
  - 构造初始化列表 `Foo() : x_(0), y_(1) {}` 解析 + 降级为
    `((struct Foo*)this)->x_ = 0; ((struct Foo*)this)->y_ = 1;`
  - 类内方法体对未限定成员访问 `n_` 自动改写为 `this->n_`
  - `obj.method(args)` 改写为 `_ZN<Cls><Len><method>E<args>(&obj, args)` 形式
    （M9 简化：仅在 `obj` 是已知类类型的局部变量时）
  - 局部类类型变量声明时显式调用默认构造 `Counter c;` →
    `struct Counter c; _ZN7CounterC1Ev(&c);`
  - 顶层 `int main() {}` 末尾追加 C 端 trampoline
    `int main() { return _ZN4mainEv(); }` 以让 libc 启动器找到 main
  - 系统头 `stddef.h` / `stdint.h` 仅在确实使用 `new` / `delete` /
    `throw` / `try` / `typeid` 时引入；否则用 `typedef unsigned long size_t;`
    替代以保证 Windows 也能用
- **DCC_CPP_KEEP_TMP**：环境变量；非空时把降级后的 C 文件写到指定路径
  而不是临时目录，便于调试

**M9 不在范围**：SFINAE / partial specialization（`partial_sort`）/
完整 concept 求值 / 协程挂起恢复 / module 分区 / 完整 GCC cc1/cc1plus
bootstrap（受限于 `dcc` 自身 C++ 库支持度与本地磁盘空间）。

**M10+ 范围**（新增，已在 `src/cpp/m10.rs` 中实现）：
- **SFINAE**（`substitution failure is not an error`）：
  - `SfinaeCtx` 记录类型形参→实参绑定，`try_substitute` 替换失败时降级
    为保留原 ident（pattern_matches 阶段判定非匹配）
  - 模板形参未绑定时不报错，模板实例化阶段跳过该候选
- **Partial specialization**：
  - 解析 `template <typename T> class Foo<T*> { ... }` 为
    `PartialSpec { params, pattern_text, decl }`
  - `pattern_matches("T*", "int*")` 识别形参/实参；支持 `T*` / `T&` /
    `const T` / 精确匹配
- **Concept 求值**（`requires { ... }`）：
  - 解析 `sizeof(T) <= N` / `same_as<T, U>` / `derived_from<T, Base>` /
    `T::value` / 表达式约束
  - `Concept::evaluate(&SfinaeCtx)` 替换形参后求值，返回 bool
- **协程挂起恢复**（C++20）：
  - `co_await expr` → `static int __coro_state = (expr);` 状态机驱动
  - `co_yield expr` → `static int __coro_yield_state = (expr); return ...;`
  - `co_return expr` → `return expr;`（带 co_return 注释）
- **Module 分区**：
  - 解析 `module foo:part1;` / `export module foo:part1;` 为
    `Module { name, partition: Some("part1"), is_export }`
  - 输出 `/* dcc C++ module (M10+): foo:part1 */` 注释
- **operator-> 重载链**：
  - 类型名含 `Ptr`/`ptr`/`Wrapper`/`Iter`/`iter` 的局部变量在 `->`
    访问时走 operator-> 链 (简化: 直接展开为 `obj->member`)
- **RTTI 完整 dynamic_cast 跨虚继承**：
  - `dynamic_cast<T*>(e)` 降级为 `((T*)__dynamic_cast((void*)(e), 0, 0, -1))`
  - Itanium C++ ABI: 通过 vtable RTTI 查找 `_ZTV<T>` 偏移
- **构造委托 / 继承链 ctor**：
  - `using Base::Base;` 解析为 `InheritingCtor { class, base }`
  - `CtorExpander::emit_inheriting(this, base)` 生成 wrapper 转发
  - 构造委托 `Foo(int) : Foo() {}` 简化为注释 (M10 简化)
- **成员函数指针** `&Class::method`：
  - 解析为 `MemberPtr { class, name }`
  - 降级为 `&_member_fn_<Class>_<method>` 形式 (M10 简化)
- **C++ ABI 线程安全 init guard** `__cxa_guard_*`：
  - 静态局部变量 `static T x = init;` 触发
  - 降级为 `_cxa_guard_acquire(&_cxa_guard_x)` + 初始化 +
    `_cxa_guard_release(&_cxa_guard_x)`
  - 状态机: 未初始化(0) → 初始化中(1) → 已初始化(2)
- **`std::initializer_list`** `{1, 2, 3}`：
  - 解析为 `InitList(elems)` / `TypedInitList { ty, elems }`
  - 降级为复合字面量 `((int[]){1, 2, 3})` 形式
  - `InitList::emit_c()` 生成 `{ ptr, count, capacity }` 结构

**M10+ 设计原则**：所有特性均降级为 dcc C 后端可直接消费的 C 代码；
对 GCC 真实 bootstrap 仍有部分受阻（受限于 dcc 自身 C++ 库 + 本地
磁盘空间），但 M10+ 显著扩展了能直接编译的 C++ 模式范围。

**限制**：C++ 子命令 (`dcc++` / `-x c++`) 在 M9 阶段已可端到端运行
简单 C++ 源（含 class / 成员函数 / 构造初始化列表 / 局部类变量 /
方法调用改写）；GCC 完整 bootstrap 仍部分受阻（M9 已解锁 C++ 端到端，
cc1/cc1plus 仍受更复杂 C++ 模式阻碍 + 本地磁盘需要为 GCC
源码约 1GB + 构建临时文件约 5GB 留足空间）；原因见
`docs/missing_features.md` 的 C++ 前端状态表。

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
- **回归**：`parallel_codegen_o2_multi_function` 断言并行/串行 `.o` 字节一致。

## GNU 控制流扩展

Parser 直接表示 `&&label`、`goto *expr` 和 `asm goto`。标签地址使用
RIP-relative LEA，计算 goto 发射间接 `jmp`，`asm goto` 在 Mini x86
汇编器之前将 `%lN` / `%l[name]` 解析到函数内标签。

M11.2 扩展：

- **跨函数 computed goto**：`&&outer_label`（其他函数的命名标签）
  通过 `lea rax, [rip + label]` 加载 64-bit 绝对地址，再经
  `jmp *rax` 间接跳转到目标函数入口；前提是目标标签 `global` 可见。
- **asm goto 输出约束增强**：`asm goto (...) : "=r"(out) : ... : : lab1, lab2`
  中 `out` 的语义遵循 GCC：仅在 fall-through 路径上写回 `out`，
  跳转到任何 `goto` 标签时 `out` 保留调用方传入的旧值。
  实现路径：跳板代码段把 `out` 当前值保存在 volatile 临时槽，
  fall-through 时 `mov` 回调用方槽，跳转路径不写回。
- **`__builtin_call_with_static_chain(callee, chain, args...)`**：
  按 SysV AMD64 ABI 把 `chain` 写入 `%r10`（静态链寄存器），
  之后按 GP 寄存器约定（`rdi/rsi/rdx/rcx/r8/r9`）和 XMM 寄存器
  约定（`xmm0..xmm5`）传递 `args`，多余参数走 caller 栈。
  适配 GNU 嵌套函数的静态链调用约定，配合 IR 一等公民的
  嵌套函数（nested function）支持。

M11.3 扩展 — 内联 asm 约束全家族：

- **约束解析**：`parse_asm_constraint_flags` 把约束字符串拆为
  `(is_output, is_inout, early_clobber, match_digit)` 四个标志。
  `is_output` 由首字符 `=` / `+` 决定；`early_clobber` 由前缀 `&` 决定；
  `match_digit` 由末尾 `0..9` 决定；`is_inout` 由 `+` 决定。
- **约束分类**：`asm_constraint_is_immediate` (i/n/I/J/K/L/M/N/O/X/E/F/G/H/s)、
  `asm_constraint_is_address` (p)、`asm_constraint_wants_xmm` (x/Y)、
  `asm_constraint_is_general` (g)、`asm_constraint_prefers_memory` (含 m 且不含 r)
  在 `asm_pick_sub_constraint` 中按 `r` > `m` > 立即数 > `g` > 默认
  优先级从多约束 ("rm") 选单约束。
- **操作数映射**：`build_asm_operand_map` 用 `Slot::{Reg, Imm, Mem}` +
  `meta: (reg, xmm_w, is_out, is_mem, flags)` 记录每个操作数。
  - GP 池：`[0, 1, 2, 6, 7, 8, 9, 10, 11]` (rax/rcx/rdx/rsi/rdi/r8..r11)。
  - XMM 池：`[0..15]`，XMM 约束 `x` / `Y` 时 `xmm_width` 记录 32/64 位。
  - `"&"` early-clobber 阶段：若输出寄存器与任一输入寄存器重叠，
    重新从 GP 池挑选并重新 emit `prepare_asm_binding` + 更新 `op_map`。
  - `"0..9"` 复用约束：复用先前操作数（`Slot::Reg`）的寄存器，
    但仍对当前操作数调 `prepare_asm_binding(expr, reg, is_output=false)`
    把值重新加载（输入语义），覆盖先前输出 prepare 写入的地址。
- **准备 / 写回**：`prepare_asm_binding(expr, reg, is_output)`
  区分语义：输出走 `lea` 取地址（配合 `writeback_asm_operand` 的
  `mov [r13+off], reg` 写回），输入直接 `gen_expr` 取值
  （修复原 bug：输入也走 lea 破坏 `r`/`+r` 实参语义）。
  `writeback_asm_operand` 对 32 位整型使用 REX.W (8 字节) 零扩展
  写回（修复原 bug：4 字节写回让 C codegen 64 位 reload 读到
  未初始化槽高 32 位的栈残留）。
- **offsetof 约束**：输入侧先尝试 `const_eval::eval` 求值，
  失败时回退到 GP 寄存器并 `lea` 加载常量基址 + 字段偏移。
- **模板替换**：`resolve_asm_template` 把 `op_map` 中的
  `%N` 占位符替换为 AT&T 形态 (`%rax` / `$305419896` /
  `-16(%r13)`)；`asm_format_subst` 对寄存器名加 `%` 前缀、
  对立即数原样 (已含 `$`)、对内存操作数原样 (含 `(`)。
- **disasm**：`reg` 函数不再叠加 REX.B（由调用方按 REX.R / REX.B /
  REX.X 决定扩展位），避免 reg 字段被错当作 r/m 字段
  扩展（之前 rax 误显示为 r8，r8 误显示为 rax）。
- **测试**：`test_asm_constraints_ext.c` 10 个用例覆盖
  i / p / g / X / 0 / & / rm / x / %= / offsetof 约束，
  本地 Linux 全 10/10 PASS。

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
- **M12 mini-asm 补全（高）** — 消除内核 paravirt / 启动代码的 nop 警告：
  - **段寄存器**：解析 `%cs`/`%ds`/`%es`/`%fs`/`%gs`/`%ss` 6 个名字成
    段号 + 0x26/0x2E/0x36/0x3E/0x64/0x65 前缀；`mov` 段寄存器支持
    `8C /r` (read) / `8E /r` (write) 两条 ModR/M 路径；`push`/`pop` 段
    走 `0x0F A0`/`0x0F A8` 系列；段寄存器 *不* 触发 REX
    编码。`SymDisp` 内存 + 段前缀直接拼接段字节在 ModR/M 之前。
  - **SymDisp 操作数**：新增 `AsmOperand::SymDisp(String, i32)`
    表示 `sym(%rip)`，写回靠 `pending_data_rels`（不再用
    `pending`，`pending` 仅给跳转用）；`jmp`/`jcc`/`call` 走
    `movabs rax, sym` + `jmp *%rax`；`setcc`/`cmpxchg`/`xadd`/
    `movzx`/`movsx`/`popcnt`/`movnt*`/`prefetch*`/`alu`/`test`/
    `shift` 全部支持 SymDisp。`parse_asm_operand_str` 处理 `*` 前缀
    把 `Sym` 升级为 `SymDisp`（间接跳转/调用）。
  - **mov 增强**：`mov %gs:mem, reg` 走 8C /r；`mov reg, %gs:mem`
    走 8E /r；`mov $imm64, mem` 走 `movabs rax, imm64` + `movq rax, mem`；
    `movabs $imm64, %rax` 直接 REX.W + B8+rd。
  - **push/pop 增强**：新增 `push`/`pushq`/`pushl`/`pushw` 与
    `pop`/`popq`/`popl`/`popw` 6 个 AT&T 扩展助记符；立即数 >8 位
    按符号扩展 32 位后 `68 id`；`pushq $imm64` 走
    `movabs rax, imm64` + `push %rax`；`push %seg` 走 0x0F A8
    1+seg*8。
  - **ALU/test 增强**：`add/sub/and/or/xor/cmp $imm, mem`
    走 `mov{abs,b} rax, imm` + ALU reg,mem；`test $imm, mem`
    走 `test imm, rax` + `mov mem, rax`；`test $imm, %al/%ax`
    直接 A8/A9。
  - **shift 增强**：`shl/shr/sar/rol/ror $cl, reg_or_mem` 当
    shift-amount 寄存器不是 `%cl`（如 `%r13`）时走
    `mov %r13, %ecx` + D3 /4 编码；`shl $imm, mem` 走
    `mov reg, mem` + `shl imm, reg` + `mov mem, reg`。
  - **setcc 增强**：AH/BH/CH/DH 高 8 位目标跳过 REX 编码（与
    gcc 行为一致；REX 会把 AH 转成 SPL 编码）；SymDisp mem 目标走
    `setcc rax` + `mov mem, al`。
  - **cmpxchg/xadd**：`cmpxchg %rax, mem` 走 `lock cmpxchg`；
    `xadd` 走 `lock xadd`；非 rax 隐式操作数走 `mov mem, %rax` +
    `lock cmpxchg` + 写回。
  - **bswap**：拒绝 16 位操作数（与 gcc 一致 — 16 位 bswap 不存在）。
  - **movnt/movntdq/movntps/movntpd**：加 NT store 编码（0F 2B /
    0F E7 等），mem 目标走 SymDisp。
  - **prefetch**：prefetcht0/t1/t2/nta 走 0F 18 /1-5；支持 SegMem
    与 SymDisp `%rip` 寻址。
  - **jmp/jcc/call 间接**：`jmp *%rax` / `call *%rax` 走 FF /4 或
    FF /2；SymDisp mem 走 `movabs rax, sym` + `jmp/call *%rax`。
  - **系统指令**：`wbinvd` (0F 09)、`invd` (0F 08)、`cpuid` (0F A2)、
    `rdtsc` (0F 31)、`rdtscp` (0F 01 F9)、`rdpmc` (0F 33)、
    `syscall` (0F 05)、`sysret` (0F 07)、`sysenter` (0F 34)、
    `sysexit` (0F 35)、`swapgs` (0F 01 F8)、`stac`/`clac` (0F 01 CB/CA)、
    `endbr64` (F3 0F 1E FA)、`endbr32` (F3 0F 1E FB)、`pause` (F3 90)、
    `hlt` (F4)、`cli`/`sti` (FA/FB)、`cld`/`std` (FC/FD)、`clc`/`stc`/`cmc`
    (F8/F9/F5)、`lahf`/`sahf` (9F/9E)、`pushf`/`popf` (9C/9D)、
    `ud2` (0F 0B)、`int3` (CC)、`lfence`/`sfence`/`mfence`
    (0F AE E8-EF)、`vmlaunch`/`vmresume`/`vmxoff` (0F 01 C4/C5/C4)、
    `vmptrld`/`vmptrst`/`vmclear` (0F C7 /6/7/6)、
    `vmread`/`vmwrite` (0F 78/79)。
  - **FPU/XMM 保存**：`fxsave`/`fxrstor` (0F AE /0/1)、
    `fnsave` (DD /6)、`fnstenv` (D9 /6)、`fnclex` (DB E2)、
    `fninit` (DB E3)、`ldmxcsr` (0F AE /2)、`stmxcsr` (0F AE /3)；
    FPU 加载走 `emit_asm_fpu_load`（带 SymDisp RIP-relative 支持）。
  - **测试**：`tests/fixtures/asm_paravirt_boot.c` 26 个用例覆盖
    段寄存器、push/pop 段、imm64、shift $cl、setcc AH、间接
    jmp、cpuid/rdtsc/wbinvd/invd、cli/sti/cld/clc/hlt、
    stac/clac、swapgs、fxsave、imm64→mem、prefetch、int3、
    pause、ud2、syscall、endbr64；`asm_paravirt_boot_compiles_-
    without_nop_fallback` 在本地 Linux 上 PASS。
  - **降级**：未识别指令仍走 `eprintln! warning` + 单字节 0x90
    (`nop`) 占位；带行号和源位置信息以方便追踪（避免静默 nop）。
- **M13 SIMD 指令子集补全（高）** — 完整实装 x86_64 SIMD 指令子集，
  覆盖加密/科学计算/机器学习等场景的内联汇编：
  - **VEX 编码基础设施**：新增 `emit_vex3_prefix` (C5)、`emit_vex4_prefix` (C4)
    通用前缀编码；`emit_asm_vex_3op_reg` / `_mem` / `_imm8` / `_imm8_check` 等
    通用 helper；AT&T 顺序 `(src1, src2, dst)` 与 imm-first 顺序 `(Imm, Reg, Reg)`
    在 helper 内部自动重排，编码符合 Intel 规范。
  - **EVEX 编码（AVX-512 简化）**：新增 `emit_evex_prefix` (62) 与
    `emit_asm_evex_3op_reg` / `_2op`；当前覆盖 zmm 大小的 vmovdqu32/64、
    vmovdqa32/64、vaddps/mulps 等基础路径；broadcast/mask/RC-SAES
    全模式为后续工作。
  - **SSE3/SSSE3/SSE4.1/SSE4.2 整数 SIMD**：`pshufb`/`pshufd/hw/lw`、
    `pmulhrsw`/`pmullw`/`pmulld`/`pmuludq`/`pmuldq`/`pmaddubsw`/
    `pmaddwd`、`pabsb/w/d`、`palignr`、`pblendw`、`phminposuw`、
    `pminub/uw/sb/ud/sd`、`pmaxub/uw/sb/ud/sd`、`pcmpeqq`、
    `pcmpgtq`、`pinsrb/d/q`、`pextrb/d/q`、`extractps`、`insertps`、
    `pmovsxbw/bd/bq/wd/wq/dq`、`pmovzxbw/bd/bq/wd/wq/dq`、
    `roundps/pd/ss/sd`、`crc32b/w/l/q`。
  - **AVX/AVX2 浮点**：`vaddps`/`vaddpd`/`vmulps`/`vmulpd`/`vsubps`/
    `vsubpd`/`vdivps`/`vdivpd`/`vminps`/`vmaxps`/`vminpd`/`vmaxpd`/
    `vsqrtps`/`vsqrtpd`/`vandps`/`vandpd`/`vorps`/`vorpd`/`vxorps`/
    `vxorpd`、`vaddss/sd`/`vmulss/sd`/`vsubss/sd`/`vdivss/sd`/
    `vminss/maxss/minsd/maxsd`/`vsqrtss/sd`、`vshufps/pd`、
    `vunpcklps/hps/lpd/hpd`、`vpermilps/pd`、`vbroadcastss/sd`/
    `i128`、`vblendps/pd`、`vperm2f128/i128`、`vinsertf128/i128`、
    `vextractf128/i128`。
  - **AVX/AVX2 整数**：`vpaddb/w/d/q`/`vpsubb/w/d/q`/`vpmullw`/
    `vpmulld`/`vpand`/`vpandn`/`vpor`/`vpxor`/`vpcmpeqb/w/d/q`/
    `vpcmpgtb/w/d/q`/`vpshufd`/`vpshufb`/`vpminub/uw/sb/ud/sd`/
    `vpmaxub/uw/sb/ud/sd`/`vpmuldq`/`vpmuludq`/`vpmulhrsw`/
    `vpabsb/w/d`/`vpaddusb/sb`/`vpsubusb`/`vpsignb/w/d`/
    `vphaddw/d/sw`/`vphsubw/d/sw`/`vpmaddwd`/`vpmaddubsw`/
    `vpavgb/w`/`vpacksswb/dw`/`vpackuswb/dw`。
  - **AVX 移动与杂项**：`vmovaps`/`vmovapd`/`vmovups`/`vmovupd`/
    `vmovdqa`/`vmovdqu`（含 128/256 位）、`vzeroupper`/`vzeroall`、
    `vpmovmskb`/`vmovmskps`/`vmovmskpd`。
  - **AVX 转换**：`vcvtps2pd`/`vcvtpd2ps`/`vcvtss2si`/`vcvtsd2si`/
    `vcvtsi2ss`/`vcvtsi2sd`。
  - **AES-NI**：`aesenc`/`aesenclast`/`aesdec`/`aesdeclast`/
    `aesimc`/`aeskeygenassist`（66 0F 38 DC-DF/DB/DE/DF 全部实装）。
  - **SHA**：`sha1rnds4`/`sha1nexte`/`sha1msg1`/`sha1msg2`/
    `sha256rnds2`/`sha256msg1`/`sha256msg2`。
  - **FMA 4 操作数（AVX/FMA）**：`vfmadd132/213/231 {ps,pd,ss,sd}`、
    `vfmsub132/213/231 {ps,pd,ss,sd}`、`vfnmadd132/213/231 {ps,pd,ss,sd}`、
    `vfnmsub132/213/231 {ps,pd,ss,sd}` 共 32 个变体；通过
    `emit_asm_fma_3op` 通用 helper 编码，AT&T 顺序 `(src1, src2, dst)`
    内部自动重排。
  - **F16C**：`vcvtph2ps`（VEX.128.66.0F38 13 /r）、`vcvtps2ph`
    （VEX.128.66.0F3A 1D /r ib）。
  - **TSX（Restricted Transactional Memory）**：`xbegin` rel16
    （C7 F8 rel16）、`xend`（0F 01 D5）、`xabort` imm8（C6 F8 ib）、
    `xtest`（0F 01 D6）；含 gcc AT&T 风格的标签相对寻址。
  - **AMX tile 指令**：`ldtilecfg`（VEX.128.0F38 49 /0）、
    `sttilecfg`（VEX.128.0F38 49 /1）、`tilezero`（VEX.128.F2.0F38 49 /0）、
    `tileloadd`（VEX.128.F2.0F38 4B /0）、`tilestored`（VEX.128.F3.0F38 4B /0）、
    `tdpbssd`/`tdpbusd`/`tdpbuud`/`tdpbf16ps`（VEX.128.F2.0F38 5E/50/51/5C）。
  - **AVX-VNNI（神经网络点积）**：`vpdpbusd`（VEX.128.66.0F38 50 /r）、
    `vpdpbssd`（VEX.128.66.0F38 51 /r）、`vpdpwssd`（VEX.128.66.0F38 52 /r）。
  - **Mask 寄存器（k0-k7）**：`kmovw`（0F 90 /r）基础路径实装。
  - **movd/movq 增强**：SSE/MMX `movd`（66 0F 6E/7E）显式
    dispatch；`movq` 走通用 `emit_asm_mov` (size=64)。
  - **2 操作数 VEX 形式**：`vunpcklps/hps/lpd/hpd` 等支持 AT&T
    2 操作数形式（src, dst），通过专用 helper 编码 vvvv=dst。
  - **测试**：`tests/fixtures/asm_avx_aes_fma_tsx.c` 30+ 个用例
    覆盖 AES-NI、SHA、FMA 132/213/231、F16C、AVX 浮点 3 操作数、
    AVX 标量、AVX 整数 3 操作数、AVX 移动与杂项、AVX-VNNI、
    SSE3/SSSE3/SSE4.1/SSE4.2（含 GPR 源/目的的 pinsr/pextr）、
    VPINSR/VPEXTR/VEXTRACT、TSX（xbegin/xabort）、AMX tile、
    AVX 转换、AVX-512 EVEX `_z` 编码、256 位 vperm2f128/vinsertf128/
    vextractf128 等；每个函数只测试一类指令子集（失败易定位）。
    `asm_avx_aes_fma_tsx_compiles_without_nop_fallback` 在本地
    Linux/Windows 上 PASS。
  - **M13.2 增量（2026-07-21）**：
    - `parse_asm_reg` 新增 `ymm0–15`/`zmm0–31`/`tmm0–7`/`k0–7`
      （u8 标签 128=xmm/129=ymm/130=zmm/131=tmm/132=k）；
    - 新增 `emit_asm_pinsr_imm8`/`emit_asm_pextr_imm8`/
      `emit_asm_vpinsr_imm8`/`emit_asm_vpextr_imm8` helper，
      正确编码 GPR/内存 ↔ XMM 的 SSE4.1/AVX 插入提取指令；
    - AMX `tileloadd`/`tilestored` 兼容两种 AT&T 操作数顺序；
    - 修复 imm8 类指令 inner 条件不匹配时静默漏发字节的 bug。
  - **已知简化**：
    - AVX-512 mask (`aaa`) / broadcast (`b`) / ER (`RC`/`RM`/`RD`/`RU`)
      路由字段目前未区分（默认 000），适用于非掩码 / 非广播场景；
      `vmovdqu32_z` 等占位编码可能与未来硬件需求不完全一致。
    - 256 位 AVX 指令在 `(Reg, Reg)` arm 中以 `l: u8` 控制 VEX.L
      位；后续可升级为 `l: u16` 统一 128/256/512。
    - `vcvtsi2ss`/`vcvtsi2sd` GPR 源操作数（32 位 vs 64 位）当前
      由 `w` 字段区分；GPR 内存源（cvt si2ss mem）未实装。
  - **降级**：少数 AVX-512 复杂指令（`vfpclassps`/`vgetmants`/`vreduceps`
    等）仍走 nop 警告；后续按需补全。
- **M13.1 x86_64 codegen 表达式层补洞（高）** — 收尾 5 个 `unimplemented_modules.md`
  第 1.2 节里长期挂账的 stub/简化，全部跑通测试用例：
  - **float 取模（`%`）软件实装**：`gen_float_binop` 检测到 `BinOp::Mod`
    不再走 `_ => return`（E1003）；改用 `divss/divsd` + `cvttss2si/cvttsd2si`
    + `cvtsi2ss/cvtsi2sd` + `mulss/mulsd` + `subss/subsd` 计算
    `l - trunc(l/r) * r`，零 libc 依赖；`ir.rs::FLOAT_BINOPS` 同步加入
    `BinOp::Mod`；`call_label("fmodf/fmod")` 路径删除（dcc 无内置链接器，
    非 `-c` 模式无法解析外部符号，新代码不再误用）。`test_float_mod.c`
    6 个用例（float/double 基本、负数、整除、嵌套）本地 PASS。
  - **int128 逻辑 `&&`/`||` 正确化**：`Expr::Binop` 优先在
    `BinOp::AndAnd/OrOr` 处对左/右操作数在 `gen_expr_decay` 后追加
    `or rdx, dst`（REX.W/REX.R 处理 dst≥8），合并 `rdx:dst`（高:低）后
    再 `test rax, rax`；避免「高 1 低 0」被旧实现误判为 0 的 bug。
    `gen_int128_binop` 中 `BinOp::AndAnd | OrOr => unreachable!` 路径
    不再被触发。`test_int128_logical.c` 5 个用例（`(__int128)(1<<64)`、
    零值、short-circuit）本地 PASS。
  - **asm `x` 约束 XMM early-clobber**：GP 与 XMM 共用 0..15 编号空间，
    旧实现只建一个 `input_reg_set` 判定冲突，XMM 路径直接 `continue`
    跳过。现在 XMM 输入加入独立的 `input_xmm_set`，`prepare_asm_xmm_binding`
    在输入/`+x` inout 时把 float 变量/表达式加载到指定 xmm（含全局
    RIP-relative movss/movsd）；`writeback_asm_operand` 按 `xmm_width`
    写回栈或全局，支持 xmm8–15；`=&x` early-clobber 在
    `xmm_pool = [0..15]` 内选未被 XMM 输入占用的 XMM。
  - **浮点字面量 rodata 解析**：原 stub 提到的「复杂 FP 字面量可能精度
    偏差」经核实是误报。当前实现用 `v.to_le_bytes()` 直接写 IEEE 754
    bit pattern 进 `.rodata`（f32 `*v as f32` 截位、long double 高 8
    字节补零），`[rip+disp32]` 加载走 `float_refs` 重定位 + patch；任意
    复杂字面量（`0x1.fffffffffffffp+1023` 等）零偏差。
  - **跨函数 LEA**：`emit_lea_r13`（`[r13+off]`，frame 内变量）、
    `emit_global_addr`（`[rip+disp32]`，全局符号/函数）、
    `Expr::String`/`Expr::Float`（RIP-relative LEA/movsd/movss）三套
    编码路径完整；`pending_data_rels`/`string_refs`/`float_refs` +
    `patch_local_jumps` 阶段在非 `-c` 模式下也能把所有
    `[rip+disp32]` 占位 0 改写为正确偏移，跨函数/跨节 LEA 均无需
    后续人工 patch。全局数组下标 `Expr::Index` 对全局符号走
    `emit_load_global_indexed`（单条 `[rip+disp32+idx*scale]` mov/movss/movsd），
    替代 LEA+ADD 两步。
  - **测试**：`tests/fixtures/float_mod.c`（6 用例）、
    `tests/fixtures/int128_logical.c`（5 用例）、
    `tests/fixtures/asm_constraints_ext.c`（含 XMM `"x"`）、
    `tests/fixtures/global_indexed.c`（全局数组 RIP+SIB）；
    `test_asm_goto_outputs.c`（3 用例覆盖 fall-through / taken / basic）本地
    全部 PASS；`test_float_lit.c`/`test_hugeval.c`/`test_nan.c` 等 12 个
    M11.2/11.3 已有测试无回归（`test_asm_constraints_ext` 在 M11.3 起即
    失败于 `test_xmm_constraint`，属 pre-existing，本 M 不在范围）。
- **M14 C 前端补洞：#pragma / attribute / atomic / MS 扩展** —
  收尾 `unimplemented_modules.md` 第 4.1 节里 8 条长期挂账的简化项，
  全链路贯穿 preproc → parser → codegen → link：
  - **`#pragma pack(n)` 影响 struct 布局**：`preproc` 维护 `pack_stack` +
    `pack_history: Vec<(u32, Option<usize>)>`，把每次 packing 变化按行号
    记录；`preprocess` 函数返回 `PreprocessOutput` 结构（取代旧 tuple
    返回），携带 `pack_history` + `weak_symbols`；`driver.rs` 把这两个
    字段塞入新加的 `ParseOptions` 传给 parser；`Parser::pack_at_line(line)`
    二分查找生效 packing；`layout_struct_with_pack` 新增
    `max_align: Option<usize>` 参数把每个字段对齐 cap 到 `min(nat, n)`；
    `parse_type_mode` 记下 `struct` 关键字所在行作为查询基准。
    优先级：`__attribute__((packed))` > `#pragma pack(n)` > 自然对齐。
  - **`_Pragma("...")` 等价 `#pragma ...`**：`_Pragma` 操作符与
    `#pragma` 复用同一条 `process_directive` 通路，自动具备
    `pack/weak/STDC/GCC diagnostic` 处理；包字符串经 `unescape_string`
    反转义。
  - **`#pragma weak name` + `__attribute__((weak))` 合并**：
    `preproc` 收集到 `weak_symbols: HashSet<String>`；`ParseOptions` 把
    集合带入 parser；`Parser::is_weak_symbol(name)` 暴露查询；
    `VarDecl` / `FuncDecl` 创建时合并 `attrs.is_weak || pragma_weak`；
    codegen 已有的 `STB_WEAK` 通路自动生效（`f.is_weak` →
    `bind: STB_WEAK`）。
  - **`__attribute__((returns_nonnull))` codegen**：
    `codegen/x86_64.rs` 新增 `current_returns_nonnull: bool` 状态；
    `compile_func` 入口处 `self.current_returns_nonnull = f.returns_nonnull`；
    `gen_stmt` 遇到 `Stmt::Return { val: Some(e) }` 时若返回类型为
    `Ptr` 或 `Func`，在 `emit_func_epilogue` 之前插入
    `test rax, rax; jnz .ok; ud2; .ok:` 序列强制非空。
  - **`__attribute__((target_clones(...)))` IFUNC resolver**：
    解析为 `FuncDecl.target_clones: Vec<String>`；链接期
    `link/target_clones.rs::TargetClonesTable::parse` 把变体名
    `foo.sse4.2` / `foo.default` 入表；`gen_irelative_relas` 写
    `STT_GNU_IFUNC` 符号 + `R_X86_64_IRELATIVE` 重定位。
    静态可执行无 CPU feature 检测时，resolver 默认返回 default
    变体地址。
  - **freestanding 模式原子宏语义修正**：`preproc` 在
    `freestanding=true` 时显式定义 `__STDC_HOSTED__=0` +
    `__STDC_NO_ATOMICS__=1`（C11 §4/6 规定），且不预置
    `atomic_store/atomic_load/atomic_exchange/atomic_fetch_*` /
    `atomic_compare_exchange_strong` 等 C11 stdatomic 宏（与 Linux
    内核 `arch/x86/include/asm/atomic.h` 等同名 inline 函数冲突，
    误展开为 `__atomic_fetch_add(i, v, 5)` 会破坏声明）。
    `__atomic_*` 内建 + `__ATOMIC_*` 常量仍保留供显式调用。
  - **MS 扩展 `__declspec` 翻译**：`parse_type_mode` 解析
    `__declspec(...)` 参数列表；`thread` → `decl_storage.is_thread_local = true`；
    `noreturn` → `decl_attrs.is_cold = true; is_noinline = true`；
    `align(N)` → `decl_attrs.alignment = max(cur, N)`；
    `dllimport` / `dllexport` 接受但无 codegen 差异。
  - **K&R 函数自动检测**：`parse_func_decl` 检测到 `(name1, name2, ...)`
    形式（参数无类型）时设 `is_old_style = true`；codegen 走与 ANSI
    相同的参数压栈路径。
  - **测试**：`pragma_family_compile_and_run` +
    `pragma_operator_compile_and_run`（覆盖 pack/weak/STDC/diagnostic）+ 
    `kr_old_style_function`（K&R 风格）+ `ms_extensions_compile_and_run`
    （__declspec 全家族）+ `attr_target_clones_compile_and_run` +
    `attr_copy_returns_nonnull_compile_and_run`（含 returns_nonnull）+
    `stdatomic_macros_hosted`（hosted 模式 atomic 宏）本地 PASS；
    freestanding 模式通过 `preproc.rs` 单元测试验证
    `__STDC_NO_ATOMICS__=1` 已设置。
- **M15 const_eval 完整化 + 运行时检查**：补全 `unimplemented_modules.md`
  第 4.2/4.3 节 7 条长期挂账项（浮点常量 / 字符串常量 / 地址常量 /
  `__builtin_*` 折叠 / constexpr / `#embed` 字节数 / 严格类型 warning）
  与 3 条运行时检查（`-fbounds-check` 数组越界 / `-ftrapv` 整数溢出
  trap / 隐式指针整数转换 warning）：
  - **`ConstValue` 枚举**：`const_eval.rs` 引入
    `Int(i128) | Float(f64) | Str { data, label } | Addr { name, offset } | Bytes { data, label }`
    五种值；`eval_full` 返回该枚举（保留旧 `eval() -> i128` API 作为
    兼容封装，仅在 `ConstValue::Float` 上做 `f as i128` 截断）。所有
    旧 parser/IR 调用点不变。
  - **浮点常量算术**：`add_values/sub_values/mul_values/div_values`
    增加 `Float<->Float` 与 `Int<->Float` 混合分支；
    `cmp_values` 把 `lt/gt/le/ge/eq/ne` 全部提升到 f64 比较（整数通过
    `as_float()` 走 i128→f64 转换；f64→i128 由 `as_int()` 负责）。
  - **字符串字面量**：`Expr::String` 直接给出 `ConstValue::Str`，label
    用 FNV-1a 64-bit 哈希（`const_str_id` 私有 helper），与 codegen
    `string_pool` 标签格式互不耦合。
  - **地址常量**：`Expr::AddrOf` 内部按子表达式分派 — `Ident` → `Addr{0}`、
    `Index{ident, idx}` → 索引常量作为 `offset`、`LabelAddr` → `Addr{0}`。
  - **`__builtin_*` 折叠**（parser 层）：`__builtin_strlen(s)` 把字符串
    字面量折叠为整数长度；`__builtin_abs/labs/llabs` 走
    `i128::wrapping_abs()`；`__builtin_bswap16/32/64` 走
    `uN::swap_bytes()`；`__builtin_expect(cond, expect) → cond`；
    `__builtin_popcount*` 走 `u64::count_ones()`。所有 builtin 折叠
    都用 `const_eval::eval_int_lit` 解析参数 + `eval_full` 验证。
  - **C23 constexpr 函数**：`parser` 在 declaration specifier 与
    `parse_type_mode` 两处识别 `constexpr` 关键字并 `advance` 跳过；
    不强制编译期求值，函数体走普通 codegen 路径。
  - **`#embed` 字节数**：preproc 已展开为 `byte0, byte1, ..., byteN-1`
    逗号分隔整数字节；`sizeof(embed_arr)` / `__builtin_strlen(embed_str)`
    走 const_eval 求长度（与普通数组无差异）。
  - **严格类型检查 warning**：parser 新增 `check_strict_conversion`，
    cast 阶段按位宽比较检测：整型收窄（`char` 接收 0x100 等）报
    `-Wint-truncation`；隐式指针↔整数转换报 `-Wint-conversion`。
    警告不阻塞编译（按 warning 类别报告），遵循 GCC 类别命名。
  - **`-fbounds-check`**：codegen `Expr::Index` 处理路径在 imul 后
    插入 `cmp rcx, n*es; jb ok; call __ubsan_handle_out_of_bounds`；
    n 溢出/0/未知大小 (`ArraySize::Var`) 时跳过检查。
  - **`-ftrapv`**：codegen `Expr::Binop` 路径末尾追加
    `jo .trap; jmp .ok; .trap: call __dcc_trapv_overflow; .ok:`；
    覆盖 `Add/Sub/Mul/Div`（x86_64 OF=1 即有符号溢出）。`__dcc_trapv_overflow`
    桩由用户链接 runtime helper（与现有 `__ubsan_*` 模式一致）。
  - **测试**：6 个新增集成测试（`m15_float_const_eval` /
    `m15_string_literal_and_concat` / `m15_address_const` /
    `m15_builtin_fold` / `m15_strict_warning_compile` /
    `m15_bounds_check_inrange` / `m15_constexpr_function`），12 个
    const_eval 单元测试（`float_arithmetic_eval` /
    `float_comparison_eval` / `int_float_mixed_eval` /
    `string_literal_eval_returns_str_value` / `address_of_ident_eval` /
    `address_of_index_eval` / `label_address_eval` /
    `legacy_eval_truncates_float_to_int` + 4 个旧测试）全部 PASS。
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
  `__builtin_va_*`（start/end/copy/arg）走 SysV va_list 结构；
  `__builtin_strcasecmp`/`__builtin_strncasecmp` 转发到 libc；
  `__builtin_fma`/`fmaf`/`fmal` 调用 libm 同名函数（fmaf/fmal 走
  F3 0F 10/11 `movss`，fma/fmal 走 F2 0F 10/11 `movsd`；
  备份 arg0 至 xmm3 避免被 i=1/2 覆盖）；
  `__builtin_rotateleft*`/`__builtin_rotateright*`（8/16/32/64 位宽
  用 ROL/ROR + cl 编码，32 位先 mov 到 eax 再回写高寄存器）；
  `__builtin_clrsb`/`clrsbl`/`clrsbll`（`clz(x ^ s_bits) - 1`，
  32-bit 用 cdq/32-bit bsr，64-bit 用 cqo/64-bit bsr；
  解决 bsr(0) 不可靠 ZF：先 `mov ecx/rcx, eax` 备份 `x ^ s_bits`，
  `bsr eax, eax` 后 `test ecx/rcx, ecx/rcx` 覆盖 ZF，
  `cmove eax, edx/rax, rdx`（sentinel=-1），
  最后 `bits-2 - eax` 得 clrsb）；
  `__builtin_overflow_p`/`__builtin_*_overflow` 用 SETO/SETB 标志；
  `__builtin_bswap*`（16 位走 rol r16, 8 + movzx 零扩）；
  `__builtin_clz`/`ctz`/`popcount`/`parity`；
  `__builtin_bitreverse*`（固定次数移位循环，无外部依赖）；
  `__builtin_frame_address`/`__builtin_return_address`（r13 帧链）；
  `__builtin_types_compatible_p`/`__builtin_choose_expr`（parser
  已折叠，codegen 兜底）；`__builtin_va_list`（类型别名）；
  `__builtin_FILE`/`__builtin_LINE`/`__builtin_FUNCTION`/
  `__builtin_FUNCSIG`/`__builtin_PRETTY_FUNCTION`；
  `__builtin_offsetof`（M11.2）支持嵌套 struct / union / 数组下标 /
  匿名 struct 成员访问，编译期折叠为字节偏移常量；
  `__builtin_huge_val`/`huge_valf`/`huge_vall`（M11.2）按 IEEE 754
  位模式 rodata 加载（double 0x7FF0000000000000 / float 0x7F800000 /
  long double 0x7FFF8000000000000000），通过 `.rodata` 重定位在
  ET_EXEC/ET_REL 模式下都正确；
  `__builtin_nan`/`nanf`/`nanl`（M11.2）走 qNaN 编码
  （double 0x7FF8000000000000，类比 huge_val 把尾数最高位置 1）；
  `__builtin_isnan`/`isinf`/`finite`/`signbit`/`fpclassify`（M11.2）
  对 NaN/Inf 位模式正确判定（`__builtin_isnan` 走 `xorps` 屏蔽符号
  位 + `ucomisd` + `setnp` + `setne`；`__builtin_isinf` 用 `andps`
  提取指数字段；`__builtin_signbit` 通过 `movmskpd` 取符号位）；
  `__builtin_call_with_static_chain`（M11.2）按 SysV ABI 把 chain
  写入 r10，并按 GP/XMM 寄存器约定（rdi/rsi/rdx/rcx/r8/r9 +
  xmm0–xmm5）传递额外参数，多余参数压栈 + 间接 call；
  `Expr::Unary Neg`（M11.2）浮点取负走恒常折叠（字面量直接生成
  -v）+ 运行时 XOR 符号位（`movabs rax, 0x8000000000000000` + 
  `movq xmm1, rax` + `pxor xmm0, xmm1`），避免之前对 `-1.0` 等
  字面量错误地走 `neg rax` 路径；
  `Expr::AddrOf` / `var_offset lea`（M11.2）REX.R 编码对 r10–r15
  修复（之前写死 `rex = 0x49` 缺少 REX.R bit，导致 r10/r11/r12/r14/
  r15 编码错位到 rdx/rcx/r8/r12/r13）；
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
- **M7 增强（v0.7+）**：
  - 真 `sdiv` / `msub` 实现整数除/取模（不再退化为 0）。
  - 真 `lslv` / `lsrv` 实现左/右移位（先前编码 bit 21 错位，Rm=wzr）。
  - 聚合类型参数 by-ref：local struct 通过 `add xN, sp, #off` 传地址。
  - 聚合类型返回值 > 16 字节：caller 分配 `<sret>` local，x0 指向 sret 槽。
  - varargs 最小：超过 8 个 GP 参数自动 spill 到 caller 栈。
  - 通用 `mov_x_reg_imm` 支持 `movz`+`movk` 64-bit 立即数装入。
- **M8 完整化（2026-07-21）**：
  - BL ±128MiB 范围校验 (`bl_offset_in_range`) + `debug_assert`
  - ADR ±1MiB 范围校验 + ADRP+ADD 退化路径 (`emit_adr_large`)
  - LDR 字面量 ±1MiB 范围校验 + 对齐检查
  - HFA（同构浮点聚合）：`classify_hfa` + `load_hfa_to_vregs` + `store_hfa_from_vregs`
  - >16B struct sret：`memcpy_sp_to_x0` 逐块拷贝
  - eh_frame / ORC / sanitizer 节：`.eh_frame` + `.orc_unwind_ip` + `.dcc.san_info`
  - DWARF / BTF：`build_dwarf_full` 用 `ArchKind::AArch64` + `.BTF` 节
  - 谓词指令：CCMP / CSEL / CSINC / CSNEG
  - 双精度浮点比较：FCMP Dn,Dm / FCCMP
  - 复杂寻址：后置/前置索引 + 扩展寄存器寻址
  - 缓存维护：dc cvac/civac/cvau/ivac + ic ivau/iallu + dsb ish/sy + isb
- **仍待完善**：NEON HFA >4 元素；大结构体嵌套返回多层调用链验证；TLS 重定位 `R_AARCH64_TLSDESC_*`。

## RISC-V 64 后端（`codegen/riscv64.rs`，M7+M8）

- **入口**：`codegen::emit` 在 `--target riscv64-linux-gnu` 时选用 `Riscv64Codegen`。
- **能力与 AArch64 对齐**：整数/浮点/向量表达式、控制流、函数调用（`R_RISCV_JAL`）、
  全局 `.rodata`、`build_elf_riscv64` / `build_elf_rel_riscv64`。
- **M7 增强（v0.7+）**：
  - 真 RV64M 指令 `div` / `divu` / `rem` / `remu`（funct7=0000001, bit 25）。
  - 真 RV64I 移位 `sll` / `srl` / `sra`（替代之前的 0）。
  - 聚合类型参数 by-ref：local struct 通过 `addi aN, sp, #off` 传地址。
  - 聚合类型返回值 > 16 字节：caller 分配 `<sret>` local，a0 指向 sret 槽。
  - varargs 最小：超过 8 个 GP 参数自动 spill 到 caller 栈。
  - 大立即数通过 `lui`+`addi` 序列；浮点运算为 RV64GC `f` 扩展子集。
- **M8 完整化（2026-07-21）**：
  - JAL ±1MiB 范围校验 (`jal_offset_in_range`) + `debug_assert`
  - HFA 分类：`classify_rv_hfa` + `HfaInfo`（float/double 1–4 元素）
  - >16B struct sret：`memcpy_sp_to_a0` 逐块拷贝
  - eh_frame / ORC / sanitizer 节：`.eh_frame` + `.orc_unwind_ip` + `.dcc.san_info`
  - DWARF / BTF：`build_dwarf_full` 用 `ArchKind::RiscV64` + `.BTF` 节
  - 压缩指令 (C 扩展)：C.ADDI/C.NOP/C.LI/C.LW/C.SW/C.J/C.BEQZ 16-bit 编码
  - FP D 扩展双精度：FLD/FSD/FADD.D/FSUB.D/FMUL.D/FDIV.D/FCVT/FSGNJ.D
  - RVV 向量基础：VSETVLI/VLE32/VSE32/VADD.VV/VMUL.VV/VREDSUM.VS
  - 特权指令/CSR：CSRRW/CSRRS/CSRRC/CSRRWI/MRET/SRET/WFI/SFENCE.VMA/FENCE
- **仍待完善**：RVV 完整指令集（宽化/窄化/掩码/浮点向量）；压缩指令自动选择；
  D 扩展完整（FSQRT.D/FMIN.D/FMAX.D/比较指令）；Zicsr 全集合（CSRRSI/CSRRCI）。

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

## Sanitizer 完整桩（`sanitizer.rs`）

Driver 在 parse 之后、IR lower 之前调用 `instrument_program()`，所有通道
都在 AST 层通过 `Expr::Comma` 注入 side-effect 调用 + `Stmt::Block` 注入
边覆盖 guard 调用实现，不需要修改 IR / codegen：

| 层级 / 通道 | 行为 | 显式不做的部分 |
|------|------|----------|
| TU init | 首个有体函数前插入 `__asan_init` / `__hwasan_init` / `__ubsan_init` / `__kcsan_init` 各一次 | 重复 init |
| 函数入口 | 每个有体函数（含嵌套函数）前插入 `__sanitizer_cov_trace_pc`（KCOV / Fuzzer）、`__kcsan_func_entry`（KCSAN） |  |
| KCOV / Fuzzer 边覆盖 | 每函数声明 `static uint32_t __dcc_cov_guards_N[16]` + 调 `__sanitizer_cov_trace_pc_guard_init` 一次；然后在 `if/while/do/for/case/label/goto` 等边位置插入 `__sanitizer_cov_trace_pc_guard(&guards[i])` | 跨函数 inline 合并 |
| KASAN / HWASAN | 任意 `Deref/Index/ArrowMember` load 前注入 `__asan_load1/2/4/8/16(&e)`；任意 LHS 为 `Deref/Index/ArrowMember` 的 `Assign` 前注入 `__asan_storeN(&lhs)`（取 LHS 的地址避免重复求值） | struct 16B 以上 / 非整型 load/store、MTE 标签 |
| UBSAN | `Div/Mod` 前 `__ubsan_handle_divrem_overflow(0, divisor)`；`Shl/Shr` 前 `__ubsan_handle_shift_out_of_bounds(0, shift_amt)`；有符号 `Add/Sub/Mul` 在非常量折叠时前插 `__ubsan_handle_signed_integer_overflow(0, lhs)` | VLA bound、null deref、float cast 等 |
| Fuzzer | 比较 `EqEq/Ne/Lt/Gt/Le/Ge/AndAnd/OrOr` 前 `__sanitizer_cov_trace_cmp1/2/4/8(a, b)`；switch dispatch 前 `__sanitizer_cov_trace_switch(val)` | 运行时拦截 / libFuzzer 协议 |
| KCSAN | 每函数 entry stub；内存访问 watchpoint 与 KASAN 共享 `__asan_loadN/__asan_storeN` 注入位 | 专用 `__kcsan_check_access` 路径 |
| 元数据 | codegen 写 `.dcc.san_info`（`DCC_SAN\0` + v2 + kasan/hwasan/ubsan/kcsan/kcov/fuzzer 各 1 字节） |  |
| `no_sanitize("address"/"undefined"/"thread"/"all")` | 按通道在函数级关闭：KASAN / UBSAN / KCSAN 通道分别被屏蔽；KCOV / Fuzzer 不受影响（coverage 仅反馈，无语义影响） |  |

CLI 解析：`SanitizerFlags::from_args` 支持
`-fsanitize=address,undefined,thread,hwaddress,fuzzer,coverage`
（含 `kernel-address` / `kernel-kcsan` 别名）与 `-fsanitize-coverage`。

已知遗留：
- dcc codegen 对 `Expr::Assign` 当前仅支持 LHS 为 `Ident`；`*p = v` /
  `a[i] = v` / `p->f = v` 的实际 store 仍是空操作，但 KASAN store 桩
  已正确插入到 store 之前（仅 codegen 部分缺失）。
- KCSAN 完整 watchpoint 仍走 KASAN 共享通道，未独立发 `__kcsan_check_access`。

回归：`sanitizer_metadata_section_emitted`、`sanitizer_prologue_in_all_functions`、
12 个 `sanitizer::tests` 单元测试。

## C 前端补洞（M11.1，`src/parser.rs` + `src/preproc.rs`）

M11.1 集中补齐 GCC/MSVC 在 C 前端的扩展语法与预处理指令，全部按
"解析 → AST 留痕 → 当前阶段不破坏语义" 的原则实现，避免在跨 TU
编译（libiberty / 内核 / GCC bootstrap）时出现语法错误。

### 解析层（`src/parser.rs`）

- **MS 扩展关键字**：`parse_type_mode` 优先匹配 `__declspec(...)` /
  `_ms_restrict` / `__pragma(...)` / `_Pragma(...)`；括号深度匹配跳过
  整个参数段（如 `__declspec(align(8))`）。
- **参数声明符**：`skip_ptr_qualifiers` 额外接受 `_ms_restrict` /
  `_restrict`，覆盖 `int * _ms_restrict dst` 这类 MS-only 限定。
- **`__attribute__` 全族**：
  - `cleanup(fn)` → `VarDecl.cleanup_fn`，由 codegen 在变量离开作用域
    时插入 `cleanup_fn(&var)`。
  - `no_sanitize(...)` → `VarDecl.no_sanitize` / `FuncDecl.no_sanitize`，
    sanitizer 阶段跳过对所有启用 sanitizer 的插桩。
  - `target_clones(...)` → `FuncDecl.target_clones`，暂存以备 IFUNC
    resolver 阶段使用。
  - `copy(fn)` → 复用 `alias_name` 通道生成别名符号。
  - `returns_nonnull` → `FuncDecl.returns_nonnull`（当前 noop 占位）。
- **`_Static_assert` 字符串化**：`const_eval::ExprDebug` 把表达式渲染
  为可读文本，失败消息附加 `evaluates to: <text>`；同时支持 C23
  单参形式 `_Static_assert(expr)`。

### 预处理层（`src/preproc.rs`）

- **`#pragma` 全家族**：`process_directive` 派发到
  `handle_pragma_pack` / `handle_pragma_weak` / `handle_pragma_stdc` /
  `handle_pragma_diagnostic`，分别维护 `pack_stack: Vec<Option<usize>>` /
  `weak_symbols: HashSet<String>` / `fenv_access: bool` / 诊断栈。
- **`_Pragma` 操作符**：宏展开阶段遇到 `_Pragma("...")` 时把内部字符串
  解码后推入 `pending_pragmas: RefCell<Vec<String>>`，在
  `flush_active_source` 中按源序批量调用 `process_directive`，避免
  `&self` / `&mut self` 借用冲突。
- **`#warning` / `#error`**：与 `_Pragma` 走同一过程；`#error` 走 fatal
  诊断中止编译，`#warning` 输出到 stderr。

### AST 留痕（`src/ast.rs`）

`VarDecl` / `FuncDecl` 新增字段：`no_sanitize: Vec<String>` /
`copy_fn: Option<String>` / `target_clones: Vec<String>` /
`returns_nonnull: bool`，现有 14 处构造点（`sanitizer.rs` /
`opt.rs` / `ssa_gvn.rs`）同步更新。

### 集成测试

`tests/integration.rs` 新增 9 条 case 覆盖全部特性；fixture 放在
`tests/fixtures/{ms_extensions,attr_cleanup,attr_target_clones,
attr_no_sanitize,attr_copy_returns_nonnull,pragma_family,
pragma_operator,user_diagnostics,static_assert_msg}.c`。

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

## 路线图 (来自 docs/roadmap.md)

M0~M10 见根目录的 task 列表。

## 优化 Pass 基础设施 M16（`src/ssa.rs` + `src/ssa_passes.rs`）

M16 阶段在 AST/typed IR 之上引入**轻量 SSA IR**，使大多数传统编译器优化
（强度削减、归纳变量、别名分析、SCCP、调度、TCO 等）有一致的工作面。
设计上遵守 suckless 原则：零外部依赖、单一文件、按"结构化函数"子集
跑全 SSA，含 goto/label/switch 的函数自动降级回原有 AST 路径。

### SSA IR 形态

```
struct SsaVar { base, version, ty, span }
enum Operand { Var, Int, Float, Str, SymAddr, FnAddr, Void, Call, Phi }
enum InstKind { Bin, Un, Cast, AddrOf, Load, Store, IndexLoad/Store,
                Call, MemberLoad/Store, Select }
enum Terminator { Ret, Jump, Branch, Unreachable }
struct BasicBlock { name, phis, insts, term, preds, succs }
struct Function  { name, params, ret_ty, entry, blocks, block_order,
                   globals, is_cold, is_hot, is_noreturn }
```

- 每个变量按 SSA 版本号命名（`x_0`/`x_1`/...），merge BB 顶端放 phi。
- `compute_dominators()` 提供经典迭代 dominator set；`reverse_postorder()`
  用于 BB 顺序。
- 转换由 `is_structured(body)` 守门：if/while/do/for/break/continue 通过；
  goto/label/switch/longjmp 拒绝。

### AST ↔ SSA 双向桥

- `ssa::build_from_func(f: &FuncDecl) -> Option<Function>`：在结构化函数上
  把函数体降级为 SSA（参数作为 version=0 的初始值）。
- `ssa::rebuild_block(func: &Function) -> ast::Block`：把优化后的 SSA 重建
  为 AST（phi 退化为对原变量在 if/else 两侧的赋值，指令序列直接用
  `Ident` 占位引用），下游 codegen 仍走 AST 路径。

### 15 个优化 Pass（M16 范围）

| # | Pass | 位置 | 关键作用 |
|---|------|------|---------|
| P1 | 跨函数 DCE | `cross_function_dce` | 从 `main` + extern 出发做 call graph BFS，删除不可达 internal static 函数 |
| P2 | 强度削减 | `strength_reduction` | `i * 2^N` → `i << N`，`i / 2^N` → `i >> N` |
| P3 | 归纳变量优化 (IVOP) | `ivop` | 标记 induction × constant 模式，给 strength_reduction 接续使用 |
| P4 | 别名分析 | `alias_analysis` | Steensgaard 风格 union-find（field-insensitive），返回 SSA 名 → set id 映射 |
| P5 | 指令调度 | `instruction_scheduling` | 局部 list scheduling：pure 运算按 dest 名字典序集中到 BB 顶端 |
| P6 | PGO 框架 | `pgo` / `load_profile` | 读 `<func>.dcc.profdata`（text 格式 `bb <name> <count>`），给 hot/cold BB 插入 `__dcc_*_hint` Store |
| P7 | 自动并行化 | `auto_parallelization` | 识别 `for.body.*` 名字 + 无 Call / 无非 induction Store 的 BB，emit `__dcc_parallel_hint` |
| P8 | 循环分块 | `loop_tiling` | 检测小循环体（≤4 inst），复制最后一条 pure 指令作为第二个 tile |
| P9 | 循环交换 | `loop_interchange` | 检测 `*.i.*` + `*.j.*` 嵌套，在 entry 插入 `__dcc_loop_interchanged` hint |
| P10 | 尾部调用优化 (TCO) | `tail_call_optimization` | 检测 exit BB 含对自身的 `Call`，告知 codegen 可改 jump |
| P11 | 函数属性传播 | `function_attribute_propagation` | `is_cold` 函数 entry 加 `__dcc_section_cold` Store 提示 |
| P12 | 寄存器分配 | `register_allocation` | 线性扫描占位实现（hint 模式，真实分配在 codegen 的硬编码 GP/XMM 池） |
| P13 | 别名集合 | `alias_set` | 三档判别（identity / disjoint / overlap），基于 P4 的 set 划分 |
| P14 | 投机执行 | `speculative_execution` | 把 BB 内 pure 指令集中到 BB 顶端，便于分支条件尽早算出 |
| P15 | SCCP | `sparse_conditional_constant_propagation` | 经典 lattice 分析（Undef/Const/Top），对纯运算做常量替换 + 删除冗余指令 |

**调度入口**（`ssa_passes::run(func, level)`）：

- `-O0` / `-O1`：跳过
- `-O2`：跑 P2, P3, P9, P8, P4, P13, P14, P11, P10, P15
- `-O3`：再加 P5, P12, P7, P6

**Driver 集成**（`opt::optimize`）：`-O2+` 在原有 typed IR 优化后追加
`cross_function_dce` + `run_ssa_passes`，后者对每个结构化函数做
AST→SSA→pass→AST 重建，再走原有的 `resync` 流程。

### 已知限制（与 suckless 定位一致）

- `auto_parallelization` 仅发 hint 提示，真正的 SIMD 代码生成由后端
  决定（暂未在 codegen 消费该 hint）。
- `register_allocation` 是 hint-only，物理寄存器仍由 codegen 的
  硬编码 GP/XMM 池负责。
- `instruction_scheduling` 是 BB 内 list scheduling，未做跨 BB 调度。
- `pgo` 暂未提供 instrumentation 路径，需要用户用 `perf record` 或
  自家 profiler 写 `<func>.dcc.profdata`。
- 含 goto/label/switch 的函数走 AST 路径，不享受 SSA pass。

### 测试

`tests/fixtures/m16_*.c` 7 个：ssa_ir / strength_reduction /
cross_function_dce / sccp / loop_opt / tco / function_attr。
`tests/integration.rs` 对应 `m16_*` 测试函数，在本地
跑 `cargo test m16_` 验证。

## M20 性能分析与插桩子系统（`src/profile/`）

PGO / GCOV / AutoFDO / LBR / MemProf / Perf 事件 / Offloading 七个
子系统的统一入口与配置：

| 子模块 | 文件 | 关键数据结构 | 关键 CLI |
|--------|------|--------------|----------|
| `profile::mod` | `src/profile/mod.rs` | `ProfileConfig` 集中开关 | — |
| `profile::gcov` | `src/profile/gcov.rs` | `GcovUnit` / `GcovFunction` / `GcovBlock` + `.gcno`/`.gcda` 二进制 | `-ftest-coverage` / `-fprofile-arcs` |
| `profile::pgo` | `src/profile/pgo.rs` | `Profraw` + `PgoSite` + `PgoWeights` + `instrument_program` | `-fprofile-generate` / `-fprofile-use` / `-fprofile-instr-generate` / `-fprofile-instr-use` |
| `profile::autofdo` | `src/profile/autofdo.rs` | `AutoFdoProfile` + `Hotness` 桶化 | `-fauto-profile=` / `-fprofile-sample-use=` |
| `profile::lbr` | `src/profile/lbr.rs` | `LbrProfile.edges` + `hot_entries` | `-flbr-profile=` |
| `profile::memprof` | `src/profile/memprof.rs` | `MemProfData` + `is_cold` 启发 | `-fmemory-profile` / `-fmemory-profile-use=` |
| `profile::perf_event` | `src/profile/perf_event.rs` | `TopdownL1` 枚举 + `PerfRecord` | `-ftopdownmetrics` |
| `profile::offload` | `src/profile/offload.rs` | `OffloadConfig` + `OffloadSection` + `OffloadKind` | `-fopenmp` / `-fcuda` / `-fhip` / `-fopenmp-targets=` / `--offload-arch=` |

第一性原理：编译器在 release/profile 路径要回答「哪里跑得慢 /
为什么是热点 / 内存分配合理吗 / 流水线饱和吗 / 要不要 offload」五
个问题，每个子系统只解决其中之一。`ProfileConfig` 把七个开关聚合
到 `cli::Config.profile`，driver 根据 `any_instrumentation()` /
`any_use()` 决定走 instrument pass 还是 use pass。

**PGO 二进制格式**（`.profraw`，`pgo::Profraw::write/read`）：
```
"PROF" 4B | version:u32 (=1) | n_sites:u32
sites[]: { func_len:u32 | func | file_len:u32 | file | line:u32 }
n_counters:u32 | counter[]: u64
```

**GCOV 二进制格式**（`.gcno` / `.gcda`，与 GCC gcov-9 layout 对齐）：
- magic `0x67636E6F` (gcno) / `0x67636461` (gcda)
- version `0x34302A` (40*)
- function_record 序列：ident / lno_sum / cfg_sum / name_len+name /
  n_bb + bb 列表（flags / succs）/ n_line + line[]
- data 加 arc tag `0x01` + u64 计数器数组

**AST 改写**（`pgo::instrument_program`）：`-fprofile-generate` 时在
每个 `Decl::Func(f)` 的 `body` 顶部插入 `BlockItem::Stmt(Stmt::Expr(
Expr::Call { __dcc_pgo_count_fn(idx) }))`，保证 PGO 计数先于 user code。

**Offload 元数据**：`OffloadSection::to_json()` 序列化为
`{"entries":[{"kind":"...","func":"...","file":"...","line":N,"device":"..."}]}`
形式，后续可换 `.dcc.offload` 二进制格式。`OffloadKind` 区分
OmpTarget / OmpTargetSimd / Cuda / Hip 四类入口。

**回归**：
- 10 个模块单元测试（`src/profile/{gcov,pgo,autofdo,lbr,memprof,perf_event,offload}.rs`）
- 11 个集成测试（`tests/profile.rs`）：`-fprofile-generate` /
  `-fprofile-use` 缺路径 / `-fprofile-arcs` / `-ftest-coverage` /
  `-frecord-profile` 缺路径 / `-fopenmp` / `-fopenmp-targets` /
  `-fcuda -fhip` / `-fmemory-profile` / `-ftopdownmetrics` /
  `-flbr-profile` 缺路径

详见 `docs/profiling.md`。

