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
   │  parser.rs)│    │  ├ aarch64.rs  │ ← typed IR lowering
   └─────┬──────┘    │  ├ riscv64.rs  │ ← typed IR lowering
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

## 类型表设计

```
~/.dcc/cache/<filehash>_<contenthash>  <-- 头文件预处理缓存
```

缓存键 = FNV-1a(文件路径) + FNV-1a(内容哈希) + FNV-1a(宏表状态),
值 = 预处理输出 + 头文件新增宏定义。
- 自定义文本格式 `DCC_PREPROCESSOR_CACHE_V7`, 零外部依赖
- 头文件仅保存自身新增的宏, 避免宏状态污染
- 内容变化时自动失效 (内容哈希不变 = 缓存命中)
- 宏表变化时 PCH 会发 `W0400` 并回退为普通预处理；PCH 文件
  不可读、损坏、写出失败或预处理错误均只发 `W0401`/`W0402`/
  `W0403` warning，不作为编译 hard failure
- `macros.is_empty()` 不再触发 `expand_macros` 早退,
  保证 `__COUNTER__` / `__FILE__` / `__LINE__` 等特殊 token
  始终进入展开路径

## QualType 类型限定符系统

仿照 Clang QualType 模式，dcc 在 AST 层引入 `QualType` 包装:

```rust
pub struct QualType {
    pub base: Type,           // 底层扁平类型
    pub quals: QualifierFlags, // const/volatile/restrict 位标志
}
```

**设计要点:**

- `Type` 枚举递归变体 (`Ptr`/`Array`/`Func`/`Complex`/`Imaginary`/
  `Vector`/`Atomic`/`Enum`) 使用 `Box<QualType>` 而非 `Box<Type>`,
  实现多级指针限定符自然传播 (`const int * volatile *`)
- `QualifierFlags` 为 `u8` 位标志: `CONST(0x01)` / `VOLATILE(0x02)` /
  `RESTRICT(0x04)` / `ATOMIC(0x08)` / `NODEREF(0x10)`
- 公共 API 接受 `&QualType`，内部 `match &qt.base` 仍对扁平 `Type`
  进行，限定符在 codegen/BTF/DWARF 阶段按需读取
- IR 层 `TypedExpr` 携带 `quals: QualifierFlags` 字段，限定符信息
  从 AST 传播到后端

**Volatile 语义保护:**

- `opt.rs`: `is_side_effect_free()` 对 volatile 类型表达式返回 `false`,
  防止死存储消除
- `ssa_gvn.rs`: `gvn_key_string()` 对 volatile 表达式返回 `None`,
  防止公共子表达式消除
- `ast::type_has_volatile()` 递归检测类型树中的 volatile 限定符

**BTF/DWARF 输出:**

- `qual_type_to_dwarf()` 在 DWARF 类型链中插入
  `DW_TAG_const_type`/`volatile_type`/`restrict_type` 节点
- BTF 输出 `BTF_KIND_CONST`/`VOLATILE`/`RESTRICT`，满足 pahole
  和 BPF CO-RE 校验需求

## 错误诊断

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
- `Target::resolve()` 不管后端实现 — 它只负责 "几何" (指针/字节序/调用约定/默认对齐),
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

架构默认对齐 (字节, `Target::base_type_align()`):

| 架构 | ptr | int | long | long long | double | long double | max_align |
|------|-----|-----|------|-----------|--------|-------------|----------|
| x86_64 | 8 | 4 | 8 | 8 | 8 | 16 | 16 |
| AArch64 | 8 | 4 | 8 | 8 | 8 | 16 | 16 |
| RISC-V 64 | 8 | 4 | 8 | 8 | 8 | 16 | 16 |
| MIPS | 4 | 4 | 4 | 8 | 8 | 8 | 8 |
| MIPS64 | 8 | 4 | 8 | 8 | 8 | 16 | 16 |
| ARM | 4 | 4 | 4 | 8 | 8 | 8 | 8 |

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
C99/GNU99+ 下未声明函数调用默认 **E0017**；C89/GNU89 或
`-fpermissive` 保留 `int` 降级 + **W1000**。dcc++ 降级到内部 C 后
会显式以 `-fpermissive` 编译，以兼容当前 C++ lowering 产生的前向调用。
hosted 模式（非 `-ffreestanding`）预置 C11 `atomic_*` 宏。

自定义代码节中的重定位会从 `.rela.text` 迁移到对应 relocation section；
未初始化的普通全局对象进入 `.bss`，不占目标文件数据空间。

## 链接与 LTO（`-r` / `-flto` / `-pie` / `-shared` / `-fbuiltin-linker`）

DCC 现已内置完整的多架构链接器（`src/link/`，支持 x86_64/AArch64/RISC-V64/MIPS64/ARM），对标 mold/lld 行为：

- **单 TU 路径**（`dcc a.c` 直出可执行文件）：单 TU 使用 `R_X86_64_PLT32`
  重定位，仅 stub PLT（不写 `.dynamic`、不生成完整链接器代码路径）。
- **多 TU 路径**（`dcc -fbuiltin-linker a.o b.o -o prog`）：启用完整
  `src/link` 链接器：ELF ET_REL 解析、跨 TU 符号解析、节合并、虚拟地址
  布局、完整重定位引擎、PLT/GOT 生成、ET_EXEC/ET_DYN 输出。
- **`-r` 模式**（合并可重定位对象）：优先使用内置路径
  `link_relocatable_builtin`（通过 `link_multi_tu` + `OutputKind::Relocatable`），
  旧路径 `link_relocatable_legacy` 保留 host `ld -r --gc-sections` 桥接。
- **`-pie` / `-shared`**：输出 ET_DYN（PIE 或 `.so`），生成 `.dynamic`、
  `.dynsym`、`.dynstr`、`.rela.dyn`、`.rela.plt`、`.plt`、`.got`、`.got.plt`。
  `-z ibt` 启用 CET/IBT 安全 PLT：PLT[0] 加 `endbr64`，函数条目放入
  `.plt.sec`（每条 32B，含 `endbr64` + `jmp *GOT` + `push $idx` + `jmp PLT0`），
  并生成 `.gnu.property` 节标记 `GNU_PROPERTY_X86_FEATURE_1_AND` (IBT/SHSTK)。
- **`-flto` IR/bitcode LTO**：每 TU 末尾在 `.dcc.lto` 节写入自定义位码
  （魔数 `DBC` + 版本 1），链接时反序列化、做跨 TU dead-code 消除。

| 组件 | 职责 | 依赖 |
|------|------|------|
| `cli.rs` | 区分 `-c` / `-r` / `-pie` / `-shared` / `-fbuiltin-linker` / `-Wl,-soname=…` | — |
| `link/mod.rs` | 链接器入口 + 多架构 `_start` 桩合成（`ensure_start_stub`）：无可执行入口时自动生成 `_start` 调用 `main` 并 `exit` | — |
| `link/object.rs` | 解析 ELF64 little-endian ET_REL 目标文件：节、符号、`.rela.*` 重定位；输入 machine 仅接受 x86_64/AArch64/RISC-V | — |
| `link/resolve.rs` | 跨 TU 符号解析：强 > 弱 > undef；隐式 undef 允许 libc weak | — |
| `link/layout.rs` | 同名节合并 + 2 段布局（`R+X` / `R+W`） + 自定义节独立 | — |
| `link/relocate.rs` | 重定位引擎：`PC32` / `PLT32` / `64` / `32` / `32S` / `GOTPCREL` / `GOT64` / `GOTOFF64` / `TPOFF64` / `TPOFF32` / AArch64（22 种）/ RISC-V（15 种） | — |
| `link/plt_got.rs` | PLT/GOT 生成、`.dynamic`、`.dynsym`、`.rela.{dyn,plt}`；CET/IBT `.plt.sec` 安全 PLT + `.gnu.property` | — |
| `link/elf_exec.rs` | ET_EXEC 输出：ELF 头、PHDR、节头、`.symtab`、`.strtab`、`.gnu.hash`；多架构包装 (x86_64/AArch64/RISC-V/MIPS64/ARM) + PT_INTERP | — |
| `link/elf_dyn.rs` | ET_DYN 输出：PIE / `.so` 共享库；多架构包装 (5 架构 × PIE/Shared) | — |
| `link/gnu_hash.rs` | `.gnu.hash` TIE 压缩（bloom filter + buckets） | — |
| `link/eh_frame.rs` | `.eh_frame_hdr` 二分搜索表 | — |
| `link/build_id.rs` | `.note.gnu.build-id` 段（SHA-1 / MD5 / UUID v4） | — |
| `link/ifunc.rs` | IFUNC（STT_GNU_IFUNC）解析 + `R_X86_64_IRELATIVE` 生成 | — |
| `link/lto_bitcode.rs` | 自有 DBC 格式 IR 位码序列化 + 跨 TU DCE / 内联 / 常量传播 / DAE + LtoConfig 配置化 + Pass 7 no_semantic_interposition | — |
| `link/legacy.rs` | `-r` 模式：优先内置路径 `link_relocatable_builtin`，旧路径 `link_relocatable_legacy` (host `ld -r`) | host `ld`（仅 legacy 路径） |
| `link/link_opts.rs` | `-z relro/now/lazy/execstack/noexecstack/ibt/shstk/pack-relative-relocs/max-page-size=N`、RPATH/RUNPATH、-fuse-ld=、-fno-semantic-interposition、SHF_GNU_RETAIN、`DCC_CET_IBT` 环境变量 | — |
| `link/symver.rs` | `.symver` 完整版本表（VERDEF / VERNEED） + `--no-undefined-version` 严格检查 | — |
| `link/target_clones.rs` | `__attribute__((target_clones(...)))` IFUNC resolver 多版本生成 | — |
| `link/fini_dispatch.rs` | `__dso_handle` / `__cxa_atexit` / `atexit` / `.fini_array` 调度 | — |

**多 TU 链接管线**（`driver.rs::run_builtin_linker`）：

1. `parse_object` 读取每个 `.o` 的节 / 符号 / 重定位（`object.rs`）。
2. `resolve` 做跨 TU 强/弱/undef 规则合并（`resolve.rs`）。
3. `LayoutBuilder` 按同名节合并并分配虚拟地址（`layout.rs`）。
4. 动态构建 PLT/GOT 与 `.dynamic` 节（`plt_got.rs`，仅 `-pie`/`-shared`）。
5. 若无可执行入口，自动合成多架构 `_start` 桩（`ensure_start_stub`，支持 x86_64/AArch64/RISC-V64/ARM）。
6. `apply_relocations` 先按当前架构白名单校验 relocation type；未枚举
   或错架构命名空间的类型返回 `UnsupportedType`，随后写入最终字节并
   把动态重定向分离到 `.rela.dyn` / `.rela.plt`。
7. `write_elf_*` 按架构分发写出 ET_EXEC / ET_DYN / ET_REL。

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

**GNU ld 兼容链接特性**（`link/link_opts.rs` + `link/symver.rs` +
`link/target_clones.rs` + `link/fini_dispatch.rs`）：

完整支持 `-z relro/now/lazy/execstack/noexecstack/max-page-size`、
`-z common-page-size`、`-z ibt/shstk`、`-rpath`/`-runpath`、`-fuse-ld`、
`--gdb-index`、`SHF_GNU_RETAIN`、`gc-sections` BFS 引用图裁剪、
`.symver` 符号版本化（含 VERDEF/VERNEED 生成）、IFUNC
`target_clones`、`__dso_handle`/`.fini_array` 调度、`-r` 可重定位输出等。
未知或参数非法的 `-z` 选项严格报错，不再静默接受。

详见 `docs/support_features.md` 链接器特性列表。

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

**运行时 dynamic loader**：dcc 生成的 PIE/动态 ELF 依赖 host `ld.so`
（设计决策），并通过 `PT_INTERP` / `.interp` 固化加载器路径；dcc 自身
可通过 musl 静态链接（见 `docs/static_build.md`），产出无 ld.so 依赖
的单文件二进制。vDSO 集成亦不在范围内。

本地验收：`cargo test link::`（50 用例）
+ `cargo test lto::`（7 用例）。

## C++ 前端（`src/cpp/`）

DCC 新增 C++ 子集前端，为 GCC bootstrap 做准备。设计原则：
**不另起 IR**，而是把 C++ AST 降级（lower）到 DCC 现有的 C 风格声明，
复用 C 前端 + 链接器整条链路。

| 文件 | 职责 |
|------|------|
| `src/cpp/lexer.rs` | C++ token 流：扩展关键字 + `::` 标点（operator/namespace/try/catch/throw/...） |
| `src/cpp/parser.rs` | class / ctor / dtor / 成员函数 / 成员变量 / `this->x` 解析 + namespace/operator/inherit/lambda/template/try-catch + partial spec/using Base::Base/init list/static local + decltype/auto/friend/noexcept/final/override/mutable/volatile/explicit/using-in-class + multi-dim subscript `obj[i,j,k]` → `CppExpr::MultiIndex` |
| `src/cpp/mangle.rs` | Itanium C++ ABI 名字修饰（`_ZN…C1E…` / `D1E` / `pl` / `_ZTV…` / `_ZTI…`） |
| `src/cpp/lower.rs` | C++ AST → C 字符串（struct + 隐式 this 形参 + `_Znwm`/`_ZdlPv` 替身 + vtable/typeinfo/lambda/exception 降级 + cxa_guard/init_list/coroutine 状态机 + 多维下标改写为 `__dcc_multi_index_N`） |
| `src/cpp/modules.rs` | C++20 Modules 占位：已知 module 表 + `import X;` 改写策略；BMF 序列化由 `src/cpp/bmf.rs` 实装 |
| `src/cpp/stdlib/headers/*` | 26 个 C++ 标准库头：cstddef/cstdlib/cstring/cstdio/string/vector/iostream/memory/exception/stdexcept/typeinfo/initializer_list/utility/algorithm/map/functional + chrono/thread/mutex/condition_variable/filesystem/expected/flat_map/regex/ranges |
| `src/cpp/m10.rs` | 高级特性降级器：SFINAE / partial spec / concept 求值 / operator-> / dynamic_cast / 构造委托 / 成员函数指针 / __cxa_guard_ / initializer_list |
| `src/cpp/template.rs` | 模板完整单态化：注册 class/function 模板及偏特化 (含 requires-clause)；`instantiate` 按需实例化 + 缓存；`pick_best_specialization` 按 match score 选择最佳偏特化；模板-id 调用表达式 `T<U>`、模板局部变量声明、typeid 模板类型 |
| `src/cpp/coroutine.rs` | 协程 Duff's device 状态机：`co_await`/`co_yield`/`co_return` 降级为 per-function `<fn>_coro_state`/`<fn>_coro_value`/`<fn>_coro_done` 变量 + switch 结构 |
| `src/cpp/bmf.rs` | Modules BMF 序列化：DCC_BMF_V1 文本格式 (module 名/import 列表/export 记录)；`ModuleInfo::to_bmf`/`from_bmf` round-trip |
| `src/cpp/gcc_compat.rs` | GCC ABI 兼容运行时：`__cxa_guard_acquire/release/abort`、`__dcc_typeinfo_t`、`__dynamic_cast`、initializer_list 布局、成员指针偏移宏；idempotent 追加到降级 C 序言 |
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
- `template <typename T> ...` → 完整单态化 (src/cpp/template.rs)：按需实例化 + 缓存 + 偏特化匹配
- `typeid(e)` / `dynamic_cast<T*>(e)` → `__dcc_typeinfo_t` 符号 / `__dynamic_cast` 辅助

C++ 前端将 C++ 特性降级为 C 代码供后端消费。已实装：

- 类/继承/虚函数/vtable/RTTI/异常处理/lambda（Itanium ABI 降级）
- 模板完整单态化 + SFINAE + 偏特化 + concept 求值
- 协程 Duff's device 状态机（`co_await`/`co_yield`/`co_return`）
- Modules BMF 序列化 + GCC ABI 兼容运行时（`__cxa_guard_*`/`__dynamic_cast`）
- 构造委托/继承链 ctor/成员函数指针/`std::initializer_list`
- CLI 集成（`-x c++`/按扩展名自动识别）、C++20 占位降级

设计原则：所有特性均降级为 C 后端可直接消费的 C 代码；
已通过 496 单元测试 + 255 集成测试。

限制：GCC 完整 bootstrap 仍部分受阻（cc1/cc1plus 受更复杂 C++ 模式阻碍）；
原因见 `docs/missing_features.md` 的 C++ 前端状态表。

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

## 多文件并行编译（`driver.rs`）

- **触发**：`dcc -c a.c b.c ...`（或 `-S`/`-E`/`-fsyntax-only`）多源文件输入；
  内置链接器路径 `dcc a.c b.c -pie` 多源文件输入。
- **CLI**：`-j<N>`/`--jobs=<N>` 控制 worker 数；`None`（默认）= 
  `available_parallelism()`；`Some(1)` 强制串行。
- **机制**：`compile_multiple_files()` 使用 `std::thread::scope` + 原子 job 索引
  work-stealing；每个 worker clone `Config`、设单文件输入 + 派生输出名、
  调用 `run()` 完成完整单文件编译流程。
- **输出命名**：每个输入文件独立派生（`foo.c` → `foo.o`、`-S` 时 `foo.s`）；
  `-o` 与多输入互斥（GCC 行为）。
- **内置链接器**：`run_builtin_linker()` 中多源文件同样并行编译为临时 `.o`
  后再进入链接阶段。
- **确定性**：并行与串行模式产出字节一致的 `.o` 文件。
- **回归**：`multi_file_parallel_compile` / `multi_file_parallel_byte_identical`
  验证多文件并行正确性。

## 增量编译（`incremental.rs` + `driver.rs`）

- **触发**：`-fcached` / `--incremental` 标志启用。
- **原理**：编译是纯函数（源 + 标志 → 目标）。若输入未变，输出无需重算。
- **指纹计算**：对 (源文件内容 + 影响输出的编译标志) 计算 SipHash 指纹。
  影响输出的标志包括：`-O`、`-std`、`--target`、`-g`、`-c`/`-S`、
  `-mcmodel=kernel`、`-mno-sse`、`-fsanitize=*`、`-D` 宏定义、`-I` 路径等。
- **缓存存储**：`.dcc-cache/` 目录，每个源文件对应一个条目，
  文件名 = 源路径的哈希，内容 = 当前编译指纹。
- **判定逻辑**：
  1. 输出文件不存在 → 必须编译
  2. 缓存条目不存在 → 首次编译
  3. 缓存指纹与当前不同 → 源或标志变了，重编译
  4. 指纹一致 → 跳过编译（cache hit）
- **适用范围**：仅 `-c`/`-S` 等产出目标文件的模式；
  `-E`/`-M`/`-MM`/`-fsyntax-only`/stdin/管道输出 不参与增量缓存。
- **与多文件并行的关系**：`-fcached` 与 `-j<N>` 正交，
  多文件并行编译中每个文件独立走增量检查。
- **回归**：`incremental_second_compile_skipped` /
  `incremental_source_change_triggers_recompile` 等集成测试验证。

## CPU 指令集特性动态检测（`cpuid.rs` + `target.rs`）

- **原理**：x86_64 平台通过 CPUID 指令在运行时检测 CPU 支持的指令集特性，
  替代硬编码默认值。非 x86 平台返回空特性集。
- **检测范围**：
  - CPUID leaf 1 ECX: SSE3, SSSE3, SSE4.1/4.2, AVX, FMA, AES, POPCNT 等
  - CPUID leaf 7 EBX: AVX2, BMI1/2, ERMS, AVX-512F/DQ/CD/BW/VL 等
  - CPUID leaf 7 ECX: CET-SS, AVX-512 VBMI/VBMI2/VNNI/BITALG 等
  - CPUID leaf 7 EDX: CET-IBT, AVX-512 4VNNIW/4FMAPS
- **OS 状态检查**：AVX/AVX-512 特性不仅需 CPU 支持，还需 OS 通过
  XCR0 寄存器启用相应状态保存 (XGETBV 检测)。
- **CLI 覆盖**：用户可通过 `-mavx`/`-mno-avx`、`-mavx2`/`-mno-avx2`、
  `-mavx512f`/`-mno-avx512f`、`-mfma`、`-mcet` 等标志显式覆盖检测结果。
  高级特性隐含低级依赖 (如 `-mavx2` 隐含 `-mavx`)。
- **Target 集成**：`Target::resolve()` 对 x86_64 目标调用
  `CpuFeatures::detect()` 填充 `cpu_features` 字段；
  `apply_cpu_feature_overrides()` 在 driver 中应用 CLI 覆盖。
- **Verbose 输出**：`-v` 模式下输出 `dcc: cpu features: sse3,...` 列表。
- **与增量编译的关系**：CPU 特性覆盖参与增量编译指纹计算，
  改变 `-mavx` 等标志会触发重编译。

## GNU 控制流扩展

Parser 直接表示 `&&label`、`goto *expr` 和 `asm goto`。标签地址使用
RIP-relative LEA，计算 goto 发射间接 `jmp`，`asm goto` 在 Mini x86
汇编器之前将 `%lN` / `%l[name]` 解析到函数内标签。

扩展：

- **跨函数 computed goto**：`&&outer_label`（其他函数的命名标签）
  通过 `lea rax, [rip + label]` 加载 64-bit 绝对地址，再经
  `jmp *rax` 间接跳转到目标函数入口；前提是目标标签 `global` 可见。
- **asm goto 输出约束增强**：`asm goto (...) : "=r"(out) : ... : : lab1, lab2`
  中 `out` 的语义遵循 GCC：仅在 fall-through 路径上写回 `out`，
  跳转到任何 `goto` 标签时 `out` 保留调用方传入的旧值。
  实现路径：asm 执行后立即发射输出写回代码（与 `emit_inline_asm` 相同）；
  fall-through 时写回代码执行，跳转时控制流跳过写回代码。
  跨函数 `&&label` 作为 asm goto 输入操作数时触发 codegen 警告。
- **`__builtin_call_with_static_chain(callee, chain, args...)`**：
  按 SysV AMD64 ABI 把 `chain` 写入 `%r10`（静态链寄存器），
  之后按 GP 寄存器约定（`rdi/rsi/rdx/rcx/r8/r9`）和 XMM 寄存器
  约定（`xmm0..xmm5`）传递 `args`，多余参数走 caller 栈。
  适配 GNU 嵌套函数的静态链调用约定，配合 IR 一等公民的
  嵌套函数（nested function）支持。

扩展 — 内联 asm 约束全家族：

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
  - **Clobber 寄存器排除**：`parse_clobber_registers` 解析 clobber 列表，
    把被 clobber 的 GP/XMM 寄存器加入 `input_reg_set` / `input_xmm_set`，
    操作数分配时自动避让。支持：
    - `"cc"` 条件码 (x86 静默接受)
    - `"memory"` 编译器屏障 (调用方发 mfence)
    - `"st"`/`"st(N)"` x87 FPU 栈寄存器 (计数但不进池)
    - `"xmm0"`..`"xmm15"` 从 XMM 池排除
    - GP 寄存器名 (如 `"rax"`/`"rbx"`/`"eax"` 等所有子宽度) 从 GP 池排除
  - `"&"` early-clobber 阶段：若输出寄存器与任一输入寄存器（含 clobber 排除集）重叠，
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
  `-16(%r13)`)；`asm_format_subst_safe` 对寄存器名加 `%` 前缀、
  对立即数原样 (已含 `$`)、对含运算符的立即数加括号保护优先级；
  `asm_reg_modifier_subst` 处理 `%b/%w/%h/%k/%q` 子寄存器修饰符；
  `%a` 地址形式 (寄存器同名, 立即数/内存去前缀)；
  `%P` 原始符号名 (去 `%` / `$` 前缀)；
  `asm_replace_label_idx` 边界安全替换 `%lN` asm goto 标签。
- **disasm**：`reg` 函数不再叠加 REX.B（由调用方按 REX.R / REX.B /
  REX.X 决定扩展位），避免 reg 字段被错当作 r/m 字段
  扩展（之前 rax 误显示为 r8，r8 误显示为 rax）。
- **x87 FPU 约束 (f/t/u)**：`asm_constraint_wants_x87` 检测 `f`/`t`/`u`
  约束，映射到 `xmm_width` 200 (st0) / 201 (st1)。
  `prepare_asm_x87_binding` 用 `fld [r13+off]` 推栈加载输入值；
  `writeback_asm_operand` 用 `fstp [r13+off]` (D9/DD) 弹栈写回。
  `+t` 读写约束先 fld 加载再 fstp 写回。
- **MMX 约束 (y)**：`asm_constraint_wants_mmx` 检测 `y` 约束，
  映射到 `xmm_width` 202。`prepare_asm_mmx_binding` 用
  `movq/movd mm, [r13+off]` (0F 6F/6E) 加载输入；
  `writeback_asm_operand` 用 `movq/movd [r13+off], mm`
  (0F 7F/7E) 写回。mini-asm 扩展 `por`/`pand`/`pxor`/
  `padd*`/`psub*`/`pcmpeq*`/`pcmpgt*` 等 MMX 指令。
- **GCC 条件码约束 (@cc*)**：`asm_parse_cc_constraint` 提取
  `@cc` 后的条件码字母序列，`cc_letter_to_setcc_opcode` 映射
  全部 16 条件码 (o/no/b/ae/e/ne/be/a/s/ns/p/np/l/ge/le/g)
  到 setcc 操作码 (0F 90+off)。不可见操作数不在模板中替换。
  `writeback_asm_operand` 用 `push rax; setcc %al; movzx %al, %eax;
  mov %rax, [r13+off]; pop rax` 写回（push/pop 保护其他输出操作数）。
- **显式子寄存器约束 ({regname})**：`asm_parse_explicit_reg` 解析
  `{eax}`/`{cx}`/`{cl}`/`{gs}` 等，返回 `ExplicitRegInfo`。
  `parse_reg_name_bits` 映射全部 GP 子寄存器名到 (idx, bits, att_name)。
  GP 约束映射到 `xmm_width` 205-208 (8/16/32/64 位)，
  段寄存器映射到 `xmm_width` 203。写回按宽度假不同大小的 store。
- **+r 读写约束修复**：`prepare_asm_binding` 对 `+r` (`is_inout=true`)
  使用 `is_output=false` 加载变量值（而非 lea 地址），
  确保 asm 模板读到正确的输入值。
- **约束类型校验**：`codegen::validate_inline_asm_constraints` 在 IR
  降级后遍历 `Decl::GlobalAsm` / `Stmt::AsmGoto` / `Expr::AsmExpr`，
  按 matched constraint、多重约束、显式寄存器和 x87/MMX/XMM 分类验证
  操作数类型；不兼容时统一发 `W0005` warning 并继续编译，保持 dcc
  兼容策略（GCC 对同类输入通常报 `impossible constraint` error）。
- **测试**：`test_asm_constraints_ext.c` 10 个用例覆盖
  i / p / g / X / 0 / & / rm / x / %= / offsetof 约束，
  本地 Linux 全 10/10 PASS。`asm_constraint_ext2.c` 19 个用例覆盖
  显式子寄存器 / @cc 条件码 / x87 f/t/u / MMX y / 段寄存器约束，
  全 19/19 PASS。

Mini x86 汇编器（`codegen/x86_64.rs`）要点：

- **标签命名空间**：C 层 `Stmt::Label` 与 asm 字符串内命名标签（如 `found:`）
  共用 `Codegen::label()`；`-O2` DCE 在 `return` 后仍保留可达标签。
- **同行标签**：`N:insn`（GNU 局部数字标签）与 `found: ret` 在发射前拆成
  标签行 + 指令行（`expand_label_insn_lines`）。
- **局部数字标签**：`6:` / `6f` / `6b` 映射为内部符号（`AsmLocalLabels`）。
- **AT&T 行内注释**：`strip_asm_comment` 剥离 `#` 后注释，避免 asm goto
  `jmp %l[x] # objtool NOPs this` 将注释误当作标签名。
- **模板转义**：`%%` 在 `%N` 占位符替换后折叠为 `%`（GNU 内联 asm 寄存器字面量）。
- **高级 asm 指令预处理器**：`AsmDirectiveExpander` 在操作数替换之前对 asm 字符串进行文本级展开：
  `.rept N`/`.endr` 重复块、`.irp sym, vals`/`.irpc sym, chars` 迭代块、
  `.macro name args`/`.endm` 宏定义与调用（`\arg` 参数替换）、
  `.include "file"` 文件包含（缺失静默跳过）、
  `.if`/`.ifdef`/`.ifndef`/`.else`/`.endif` 条件汇编（简单整数表达式求值）、
  `.intel_syntax`/`.att_syntax` 语法切换透传、`.altmacro`/`.noaltmacro` 模式透传。
  `asm_output` 属性通过 `FuncDecl::asm_output_template` 存储并在属性合并时处理。
- **指令子集**：除 mov/alu/jcc 外，含 `bt/bts/btr/btc`、`bsf/bsr/popcnt`、`bswap`、
  `movnti(l/q)`、`vmcall/vmmcall`、`shld/shrd`、`adc/sbb`、
  `cmpxchg`/`xadd`/`cmov` 内存操作数、`movzbl` 内存源、`setcc` 内存目标、
  `push`/`pop` 内存操作数、`lea symbol(%rip)`、GAS 伪指令
  （`.pushsection`/`.balign`/`.long`/`.quad`/`.popsection` 等，`-c` 静默填充）、
  `lock`/`rep` 前缀；`cmp`/`test`/shift/imul/unary 内存操作数、
  `rol`/`ror`、`prefetcht0/t1/t2/nta`、`movss`/`movsd`/`addss`/`addsd`、
  `xorps`/`xorpd`、`swapgs`/`invlpg`/`int3` 等；
  未识别指令产生 E0311 hard error (P0-5: 不再 warning + nop 占位)。
详见 `docs/support_features.md` 汇编与 SIMD 指令列表。
- **C 前端补洞**：`#pragma pack/weak/STDC/diagnostic` 全家族、`_Pragma` 操作符、
  `__attribute__((packed/weak/returns_nonnull/target_clones/cleanup/no_sanitize/copy/...))` 全族、
  `__declspec` MS 扩展翻译、freestanding 原子宏语义修正、K&R 函数自动检测、
  `_Static_assert` 字符串化（含 C23 单参形式）。
- **const_eval 完整化 + 运行时检查**：`ConstValue` 五类型枚举（Int/Float/Str/Addr/Bytes）、
  浮点/字符串/地址常量算术、`__builtin_*` 编译期折叠、C23 constexpr、
  严格类型检查 warning（`-Wint-truncation`/`-Wint-conversion`）、
  `-fbounds-check` 数组越界、`-ftrapv` 有符号溢出 trap。

详见 `docs/support_features.md` C 前端与常量求值特性列表。
- **SIMD/复数**：向量全局 `{…}` 逐元素 `.data`/`.rodata` 静态初始化（`gen_global_init_list`）；
  局部/rvalue 经帧底临时区 slot 传递；**标量逐元素** `+/-/*`（整数用 GP，
  浮点用标量 SSE，不用 XMM 向量指令）；
  向量 `arr[i]` 读写经 LEA 取栈/全局基址再按元素偏移寻址（非指针 load）；
  非 Ident 向量（binop、compound literal、InitList）经 `gen_vector_to_temp` 物化；
  同尺寸 vector/scalar、vector/vector、complex/vector cast 走帧内 bitcast 临时槽；
  `_Complex` add/sub/mul/div（SSE 标量）；`parser` 对复数二元结果保留 `Type::Complex` 以便
  `(int)(a+b)` 等 Cast 正确发射 `cvttss2si`；虚部清零用 `movss` 避免 64 位
  store 覆盖相邻实部；`mov_xmm` 仅在 xmm≥8 时加 REX。不支持表达式路径输出
  带源位置 warning，并发射 **ud2** 而非静默 `mov 0`。
- **Builtins**：完整支持 `__builtin_constant_p`/`__builtin_object_size`/
  `__builtin_va_*`/`__builtin_prefetch`/`__builtin_fma*`/
  `__builtin_rotateleft*`/`__builtin_rotateright*`/`__builtin_clrsb*`/
  `__builtin_overflow_p`/`__builtin_bswap*`/`__builtin_clz`/`ctz`/
  `popcount`/`parity`/`bitreverse*`/`__builtin_frame_address`/
  `__builtin_return_address`/`__builtin_types_compatible_p`/
  `__builtin_choose_expr`/`__builtin_offsetof`/`__builtin_huge_val*`/
  `__builtin_nan*`/`__builtin_isnan`/`isinf`/`finite`/`signbit`/
  `fpclassify`/`__builtin_call_with_static_chain` 等。
  未知 `__builtin_*` warning + 零值（有返回值）或 **ud2**（void）。

  详见 `docs/support_features.md` Builtins 列表。
- **IR 内联**：`-O0+` 对 `always_inline` 单返回体内联；`-O1+` 完整 pass；
  `-O3` opportunistic 小 static 函数；`noinline` 阻止内联。
- **SSA/GVN（`-O2+`）**：块内 GVN/CSE、拷贝传播、冗余局部合并。
- **循环优化**：`-O2+` LICM；`-O3` 小 trip 展开与向量元素循环向量化。

## AArch64 后端（`codegen/aarch64.rs`）

- **入口**：`codegen::emit` 在 `--target aarch64-linux-gnu` 时选用 `Aarch64Codegen`。
- **IR lowering**：遍历 `Module.functions` 与 `TypedExpr`；语句（`if`/`while`/`for`/`return`）
  对照 AST 与 `TypedStmt.nested` 保持控制流语义。
- **ABI**：AAPCS64 整数/浮点/向量（基础 NEON `v0.4s`）表达式、
  `bl` 函数调用（`R_AARCH64_CALL26`）、聚合类型参数 by-ref、
  聚合返回值 > 16 字节走 sret、varargs 最小 spill。
- **已实现**：`sdiv`/`msub` 整数除/取模、`lslv`/`lsrv` 移位、
  `movz`+`movk` 64-bit 立即数、BL/ADR/LDR 范围校验、
  HFA 同构浮点聚合分类、eh_frame/ORC/sanitizer 节、
  DWARF/BTF 调试信息、CCMP/CSEL/CSINC/CSNEG 谓词指令、
  双精度浮点比较、复杂寻址模式、缓存维护指令。
- **全局 `.rodata`**、`ET_REL` `.o` 与 `ET_EXEC` hello
  （`_start` → `bl main` → `svc #0`）；`-S` 输出 GNU 汇编骨架。
- **仍待完善**：NEON HFA >4 元素；大结构体嵌套返回多层调用链验证；TLS 重定位 `R_AARCH64_TLSDESC_*`。

## RISC-V 64 后端（`codegen/riscv64.rs`）

- **入口**：`codegen::emit` 在 `--target riscv64-linux-gnu` 时选用 `Riscv64Codegen`。
- **能力与 AArch64 对齐**：整数/浮点/向量表达式、控制流、函数调用（含浮点返回值保留在 `fa0`，`R_RISCV_JAL`）、
  全局 `.rodata`、`build_elf_riscv64` / `build_elf_rel_riscv64`。
- **ABI**：RV64M 整数除/取模、RV64I 移位、聚合类型参数 by-ref、
  聚合返回值 > 16 字节走 sret、varargs 最小 spill、
  大立即数 `lui`+`addi` 序列。
- **已实现**：JAL 范围校验、HFA 分类、eh_frame/ORC/sanitizer 节、
  DWARF/BTF 调试信息、压缩指令 (C 扩展) 16-bit 编码 + 自动 4→2 字节压缩优化
  (C.ADDI/C.LI/C.LUI/C.MV/C.ADD/C.LW/C.SW/C.LD/C.SD)、
  FP D 扩展双精度、RVV 向量基础（VSETVLI/VLE32/VSE32/VADD.VV/VMUL.VV）、
  特权指令/CSR（CSRRW/CSRRS/CSRRC/MRET/SRET/WFI/SFENCE.VMA/FENCE）。
- **仍待完善**：RVV 完整指令集（宽化/窄化/掩码/浮点向量）；
  D 扩展完整（FSQRT.D/FMIN.D/FMAX.D/比较指令）；Zicsr 全集合（CSRRSI/CSRRCI）。

## 测试脚本布局

活跃验收脚本见 `scripts/README.md`；历史调试脚本归档于 `scripts/archive/`。

预处理 `#` 字符串化遵循 C 规则：实参内换行与连续空白折叠为单空格
（`NOFAIL(ptr)` → `"malloc(...)"` 单行字面量）。

## Typed IR 与语义校验（`ir.rs`）

Driver 在 codegen 前执行 `lower → validate → opt → resync → validate`：

| 阶段 | 职责 |
|------|------|
| `lower` | AST 降级为带 `ValueType` 的函数/全局图；全局初值过 `is_static_initializer`，违例报 **E1006**；另含 **E1001**（未解析类型）、**E1004**（非法解引用）、**E1005**（va_arg 非 va_list）、**E1009**（未知成员）、**E1010**（非聚合成员访问） |
| `validate` | 校验 float/vector/complex 二元/一元/赋值/条件/聚合/转换是否与 codegen 能力一致；float/vector/complex binop、unop、cast 均只允许显式枚举集合。报 **E1002**（函数无名）、**E1003**（不支持的浮点运算/转换）、**E1007**（向量运算/赋值/转换）、**E1008**（复数运算/转换） |
| `opt` | 常量折叠、`-O0` 仍内联 `always_inline`、`-O1+` 函数内联、
  `-O2+` GVN/CSE/拷贝传播/LICM、`-O3` 循环展开与向量循环向量化；改写 AST |
| `resync` | opt 后从 AST 重新 lower，使 IR 图与优化结果一致 |
| `validate`（二次） | resync 后再次语义校验 |

x86_64 后端 `compile(&Module)` 与 AArch64/RISC-V 一致：从 `Module::source()` 读 AST，
同时 IR 图供 debug/LTO/并行 codegen 元数据使用。

Linux jump_label 预处理残留 `_DECLARE_STATIC_KEY_*` 在 parser 层由
`parse_static_key_decl()` 补全为 `extern struct static_key_{false,true}`，
再进入上述 IR 管线。回归：`tests/fixtures/c/skey_*.c`、`ir::tests`。

## Sanitizer 完整桩（`sanitizer.rs`）

Driver 在 parse 之后、IR lower 之前调用 `instrument_program()`，所有通道
都在 AST 层通过 `Expr::Comma` 注入 side-effect 调用 + `Stmt::Block` 注入
边覆盖 guard 调用实现，不需要修改 IR / codegen：

| 层级 / 通道 | 行为 | 显式不做的部分 |
|------|------|----------|
| TU init | 首个有体函数前插入 `__asan_init` / `__hwasan_init` / `__ubsan_init` / `__kcsan_init` 各一次；内核模式（`kernel-address` / `kernel-hwaddress`）不发射 `__asan_init` / `__hwasan_init`（内核自初始化 KASAN） | 重复 init |
| 函数入口 | 每个有体函数（含嵌套函数）前插入 `__sanitizer_cov_trace_pc`（KCOV / Fuzzer）、`__kcsan_func_entry`（KCSAN） |  |
| KCOV / Fuzzer 边覆盖 | 每函数声明 `static uint32_t __dcc_cov_guards_N[16]` + 调 `__sanitizer_cov_trace_pc_guard_init` 一次；然后在 `if/while/do/for/case/label/goto` 等边位置插入 `__sanitizer_cov_trace_pc_guard(&guards[i])` | 跨函数 inline 合并 |
| KASAN / HWASAN | 任意 `Deref/Index/ArrowMember` load 前注入 `__asan_load1/2/4/8/16(&e)`（>16B 走双参 `__asan_loadN(addr, size)`）；任意 LHS 为 `Deref/Index/ArrowMember` 的 `Assign` / `PreInc/PreDec/PostInc/PostDec` 前注入 `__asan_storeN(&lhs)`（取 LHS 的地址避免重复求值）；codegen 级栈红区：`pre_register_vars` 每变量后追加 4×8B Void 占位槽 (32B redzone)，帧分配后 `__asan_poison_memory_region` 毒化全帧 + `__asan_unpoison_memory_region` 逐变量解除毒化；全局变量 .rodata 后追加 32B 零填充 | MTE 标签 |
| KASAN-kernel（内联 shadow） | `-fsanitize=kernel-address` 不调 `__asan_loadN/__asan_storeN`，改发内联 shadow 检查：`need = (addr & 7) + size`，逐 granule 读 `*(u8*)(shadow + (addr >> 3) + g)`，`v >= 128 || (v > 0 && need - 8g > v)` 即毒化，调用 `__asan_report_loadN/storeN(addr)`（size 感知 + granule 边界感知，shadow 偏移按架构分发：x86_64 `0xdffffc0000000000` / AArch64 `0xdfff600000000000` / RISC-V 64 `0xdffffe0000000000`） |  |
| UBSAN | `Div/Mod` 前 `__ubsan_handle_divrem_overflow(0, divisor)`；`Shl/Shr` 前 `__ubsan_handle_shift_out_of_bounds(0, shift_amt)`；有符号 `Add/Sub/Mul` 在非常量折叠时前插 `__ubsan_handle_signed_integer_overflow(0, lhs)`；VLA bound、null deref、float→int cast overflow 均插 `__ubsan_handle_*` |  |
| Fuzzer | 比较 `EqEq/Ne/Lt/Gt/Le/Ge/AndAnd/OrOr` 前 `__sanitizer_cov_trace_cmp1/2/4/8(a, b)`；switch dispatch 前 `__sanitizer_cov_trace_switch(val)` | 运行时拦截 / libFuzzer 协议 |
| KCSAN | 每函数 entry stub + `__kcsan_func_entry`；每次访存独立发 `__kcsan_check_access(addr, size, is_write)`，与 KASAN 相互独立可共存（KCSAN 查数据竞争，KASAN 查越界/UAF） |  |
| 元数据 | codegen 写 `.dcc.san_info`（`DCC_SAN\0` + v2 + kasan/hwasan/ubsan/kcsan/kcov/fuzzer 各 1 字节） |  |
| `no_sanitize("address"/"undefined"/"thread"/"all")` | 按通道在函数级关闭：KASAN / UBSAN / KCSAN 通道分别被屏蔽；KCOV / Fuzzer 不受影响（coverage 仅反馈，无语义影响） |  |

CLI 解析：`SanitizerFlags::from_args` 支持
`-fsanitize=address,undefined,thread,hwaddress,fuzzer,coverage`
（含 `kernel-address` / `kernel-kcsan` 别名）与 `-fsanitize-coverage`。
宿主链接时 `-fsanitize=kernel-address` / `kernel-hwaddress` 不透传
`-fsanitize=address` / `-fsanitize=hwaddress`（内核运行时自提供）。

AArch64 MTE 硬件 tag 模式（`-fsanitize=hwaddress` + `--target aarch64-*`）：
codegen 级 `Aarch64Codegen` 在函数 prologue 发射 tag 栈帧：
`sub sp` → `irg x16, sp`（随机 tag）→ `addg sp, x16, #0, #0`（sp 携带本帧 tag，
局部地址经 `mov_x_reg_sp_off`/`[sp,#off]` 自动继承该 tag，即指针 tag 流程）
→ 逐 16B granule `stg x16, [sp, #off]` 写入 tag → `str x19` 保存入参
callee-saved x19 → `x19 = 带 tag 帧基址`。每个调用点（直接调用 / TLSDESC
blr）先 `and sp, x19, #mask` 清除 tag 再进入外部代码，返回后
`mov sp, x19` 恢复；epilogue 恢复 x19 并 `and sp, sp, #mask` 清 tag 后释放帧。
MTE 指令编码按 ARMv8.5-A 规范（与 LLVM `armv8.5a-mte.s` 已知编码逐字节对齐）。

已知遗留：
- 无（HWASAN 软件 tag 模式 + AArch64 MTE 硬件 tag 模式均已接入）。

回归：`sanitizer_metadata_section_emitted`、`sanitizer_prologue_in_all_functions`、
`kasan_stack_redzone_poison_unpoison`、`kasan_global_redzone_padding`、
`kernel_kasan_inline_shadow_report_emitted`、`kcsan_with_kasan_emits_both_checks`、
`kasan_store_check_on_postinc_deref_emitted`、`incdec_memory_lvalue_runtime`、
`mte_emitter_encodings_match_arm_spec`、`hwasan_mte_prologue_emits_tag_frame`、
`hwasan_mte_call_site_untag_retag`、`no_mte_no_tag_instructions`、
46 个 `sanitizer::tests` 单元测试。

## C 前端扩展语法（`src/parser.rs` + `src/preproc.rs`）

C 前端补齐 GCC/MSVC 扩展语法与预处理指令，按“解析 → AST 留痕 → 当前阶段不破坏语义”
原则实现，避免跨 TU 编译（libiberty / 内核 / GCC bootstrap）时出现语法错误。
主要特性：MS 扩展关键字（`__declspec`/`_ms_restrict`/`__pragma`）、
`__attribute__` 全族（`cleanup`/`no_sanitize`/`target_clones`/`copy`/`returns_nonnull`）、
`#pragma` 全家族（`pack`/`weak`/`STDC`/`diagnostic`）、`_Pragma` 操作符、
`#warning`/`#error`、`_Static_assert` 字符串化（含 C23 单参形式）。
AST 留痕通过 `VarDecl`/`FuncDecl` 新增字段（`no_sanitize`/`copy_fn`/
`target_clones`/`returns_nonnull`）实现。

## 自举

```bash
# 阶段 0: 用 rustc 编出 dcc0
cargo build --release

# 阶段 1: dcc0 编译自举测试程序 (14 组测试)
./target/release/dcc -O0 -std=gnu23 \
  tests/fixtures/c/bootstrap_selftest.c -o /tmp/selftest
/tmp/selftest   # exit 0 = 全部通过

# 阶段 2: 确定性验证 — 同一源文件编译两次, 对比 .o
cp1=$(mktemp); cp2=$(mktemp)
./target/release/dcc -c -O0 -std=gnu23 \
  tests/fixtures/c/bootstrap_selftest.c -o "$cp1"
./target/release/dcc -c -O0 -std=gnu23 \
  tests/fixtures/c/bootstrap_selftest.c -o "$cp2"
cmp -s "$cp1" "$cp2"   # 字节级确定性

# 阶段 3: 语义级对比 (--bootstrap-compare)
./target/release/dcc -c -O0 -std=gnu23 \
  --bootstrap-compare="$cp1" \
  tests/fixtures/c/bootstrap_selftest.c -o "$cp3"
# 输出 "byte + semantic identical" 即收敛

# 阶段 4: 多文件编译一致性
# 对 examples/ 下多个 C 文件各编译两次, 确认字节一致

# 一键验证: make bootstrap
```

## 路线图 (来自 docs/roadmap.md)

M0~M10 见根目录的 task 列表。

## 优化 Pass 基础设施（`src/ssa.rs` + `src/ssa_passes.rs`）

在 AST/typed IR 之上引入**轻量 SSA IR**，使大多数传统编译器优化
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

### 15 个优化 Pass

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
| P11 | 函数属性传播 | `function_attribute_propagation` | `is_cold` 函数 entry 加 `__dcc_section_cold` Store 提示；`is_noreturn` 传播到 codegen 触发函数末尾 `ud2` 插入 |
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

`tests/fixtures/opt/*.c` 7 个：ssa_ir / strength_reduction /
cross_function_dce / sccp / loop_opt / tco / function_attr。
`tests/integration.rs` 对应 `m16_*` 测试函数，在本地
跑 `cargo test m16_` 验证。

## 性能分析与插桩子系统（`src/profile/`）

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

详见 `docs/profiling.md`。

