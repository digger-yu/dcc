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
| 完整后端 (has_backend == true) | `x86_64-unknown-linux-gnu` / `aarch64-linux-gnu` / `riscv64-linux-gnu` | `codegen::emit` 真正产出机器码,ET_EXEC/ET_REL 正常写出 |
| 仅 Triple 解析 (has_backend == false) | `mips-linux-gnu` / `mips64-linux-gnuabi64` / `arm-linux-gnueabihf` (及 `armv7-…` 别名) | `target.rs::resolve()` 给出正确几何参数 (指针大小 / 字节序 / 调用约定),`cli::parse()` 完整通过;`codegen::emit()` 会 `panic!` 并通过 `Arch::backend_missing_msg()` 输出明确诊断 |

设计动机: Linux kernel 编译链常在 host 端用 `--target=...` 探测远端架构,
只解析不解码能避免误伤;同时也明确告诉用户 "我们解析了,只是没后端"。

第一性原理:
- `Arch::has_backend()` (src/cli.rs) 是单一真相点,所有调用者通过它判定
  是否能 emit。
- `Target::resolve()` 不管后端实现 — 它只负责 "几何" (指针/字节序/调用约定),
  即便后端是 parse-only 也要正确填字段,这样上层预处理、宏展开、IR
  lowering 等阶段可以无差别地基于 target 做选择,无需 `match arch`。
- `codegen::emit()` 的兜底分支直接 panic,文本来自 `backend_missing_msg()`,
  列出 "supported backends / parse-only triples" 双类清单。

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
| `link/relocate.rs` | 重定位引擎：`PC32` / `PLT32` / `64` / `32` / `32S` / `GOTPCREL` | — |
| `link/plt_got.rs` | PLT/GOT 生成、`.dynamic`、`.dynsym`、`.rela.{dyn,plt}` | — |
| `link/elf_exec.rs` | ET_EXEC 输出：ELF 头、PHDR、节头、`.symtab`、`.strtab`、`.gnu.hash` | — |
| `link/elf_dyn.rs` | ET_DYN 输出：PIE / `.so` 共享库 | — |
| `link/lto_bitcode.rs` | 自有 DBC 格式 IR 位码序列化 + 跨 TU DCE | — |
| `link/legacy.rs` | 旧 `-r` 模式（host `ld -r`） | host `ld`、`objcopy` |

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
4. `run_cross_tu_lto` 跨 TU DCE：标记未引用函数 / 全局为 dead。

**明确不做**：跨 TU 函数内联 / 常量传播 / 全局优化（仅做 DCE），
AArch64 / RISC-V 后端的 ELF 输出（沿用 host 链接器）。

远程验收：`scripts/sync_remote_build.sh`；`cargo test link::`（12 用例）。

## C++ 前端 M7（`src/cpp/`）

DCC 在 M7 阶段新增 C++ 子集前端，为 GCC bootstrap 做准备。设计原则：
**不另起 IR**，而是把 C++ AST 降级（lower）到 DCC 现有的 C 风格声明，
复用 C 前端 + 链接器整条链路。

| 文件 | 职责 |
|------|------|
| `src/cpp/lexer.rs` | C++ token 流：扩展关键字 + `::` 标点 + M8 关键字（operator/namespace/try/catch/throw/...） |
| `src/cpp/parser.rs` | class / ctor / dtor / 成员函数 / 成员变量 / `this->x` 解析 + M8 (namespace/operator/inherit/lambda/template/try-catch) + M10+ (partial spec/using Base::Base/init list/static local) |
| `src/cpp/mangle.rs` | Itanium C++ ABI 名字修饰（`_ZN…C1E…` / `D1E` / `pl` / `_ZTV…` / `_ZTI…`） |
| `src/cpp/lower.rs` | C++ AST → C 字符串（struct + 隐式 this 形参 + `_Znwm`/`_ZdlPv` 替身 + vtable/typeinfo/lambda/exception 降级 + M10+ cxa_guard/init_list/coroutine 状态机） |
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
bootstrap（受限于 `dcc` 自身 C++ 库支持度与远端磁盘空间）。

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
对 GCC 真实 bootstrap 仍有部分受阻（受限于 dcc 自身 C++ 库 + 远端
磁盘空间），但 M10+ 显著扩展了能直接编译的 C++ 模式范围。

**限制**：C++ 子命令 (`dcc++` / `-x c++`) 在 M9 阶段已可端到端运行
简单 C++ 源（含 class / 成员函数 / 构造初始化列表 / 局部类变量 /
方法调用改写）；GCC 完整 bootstrap 仍部分受阻（M9 已解锁 C++ 端到端，
cc1/cc1plus 仍受更复杂 C++ 模式阻碍 + 远端可用磁盘 2.5G 不够装 GCC
源码约 1GB + 构建临时文件约 5GB）；原因见
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
- **限制**：HFA（NEON 同构浮点聚合）尚未识别，仍按 GP by-ref 处理；大于
  16 字节的 struct 返回依赖 sret 但不验证调用者拿到数据；无 eh_frame / ORC /
  sanitizer 元数据节（仅 x86_64 具备）。

## RISC-V 64 后端（`codegen/riscv64.rs`，M7）

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
- **限制**：HFA（FP aggregate）尚未识别；无 eh_frame / ORC / sanitizer 元数据节；
  大于 16 字节 struct 返回同样依赖 sret，调用者需确保取到 sret 数据。

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
| 元数据 | codegen 写 `.dcc.san_info`（`DCC_SAN\0` + v1 + kasan/ubsan/kcsan/kcov 各 1 字节） | 运行时库内置 |

**明确未实现（需 IR/codegen 扩展）**：内存访问级 KASAN shadow、UB 条件
指令级 UBSAN、KCSAN 数据竞争 watchpoint、KCOV 边/对比计数等覆盖率细节。

CLI：`-fsanitize=address|undefined|thread|kernel-kcsan`、`-fsanitize=a,b`
组合、`-fsanitize-coverage` / `fuzzer` / `kernel-address`。完整
KASAN/UBSAN 需后续 IR 指令级插桩。

回归：`sanitizer_metadata_section_emitted`、`sanitizer_prologue_in_all_functions`、
`scripts/remote_feature_check.sh`（sanitizer 节 + 汇编桩计数）。

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
