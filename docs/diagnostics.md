# DCC 错误诊断码参考

> 最后更新：2026-07-21  
> 实现：`src/diagnostics.rs` · 回归：`tests/errors.rs` + `tests/fixtures/errors/`

本文档是 DCC **Diagnostic 错误/警告码**的权威说明。CLI 参数错误（`dcc: error: …`，无 `[Exxxx]`）不在编号体系内，见 [CLI 非编号错误](#cli-非编号错误)。

---

## 1. 输出格式

所有带编号的诊断采用 **GitHub 风格**四段式输出（终端带 ANSI 颜色，snapshot 测试剥离颜色）：

```text
error[E0006]: expected expression, got Semi
  --> main.c:3:13
   3 |     int x = ;
     |             ^
  hint: expected a value, variable, or expression here
```

| 字段 | 说明 |
|------|------|
| 严重性 | `error` / `warning` / `note` |
| 码 | 方括号内，如 `[E0006]`、`[W0001]` |
| 位置 | `file:line:col`（1-based） |
| 源码行 | gutter + `\|` + 原文 |
| 指针 | `^` 重复 `len` 次 |
| hint | 可选修复建议 |

### 1.1 颜色控制：`-fdiagnostics-color=<policy>`

| 取值 | 行为 |
|------|------|
| `always` | 始终输出 ANSI 转义序列（强制上色，便于 `tee` / `less -R` 配色） |
| `never` | 始终不输出（适合重定向到日志/文件/CI artifact） |
| `auto` | 仅当 stderr 是 TTY 时输出（**默认**；`std::io::IsTerminal`） |

非法值会以 `dcc: error: invalid -fdiagnostics-color value: …` 拒绝并退出码 2。  
覆盖所有错误路径（CLI 错误 / Driver 错误 / 模板树提示），保证 `tee`、`less -R`、CI artifact 行为一致。

### 1.2 输出格式：`-fdiagnostics-format=<style>`

| 取值 | 形态 | 典型首行 |
|------|------|----------|
| `github` | 默认 GitHub 四段式 | `error[E0006]: expected expression, got Semi` |
| `clang` | Clang 风格（去掉 `[E0006]` 括号） | `error: expected expression, got Semi` |
| `msvc` | MSVC 风格 | `main.c(3,13): error E0006: expected expression, got Semi` |
| `vi`    | 单行（无 caret / 无 hint） | `main.c:3:13: error[E0006]: expected expression, got Semi` |
| `sarif` | SARIF 2.1.0 JSON（写到 stderr） | `{"$schema": … "version": "2.1.0" …}` |

非法值会以 `dcc: error: invalid -fdiagnostics-format value: …` 拒绝并退出码 2。  
SARIF 输出经 `render_sarif` 统一序列化，消息体走 JSON 转义（`"` `\` 控制字符等）。

### 1.3 模板树：`-fdiagnostics-show-template-tree`

开启后，若诊断消息内含 `Foo<…, Bar<…>>` 形式的尖括号嵌套，将额外输出 `  template-tree:` 折叠视图（按层级缩进展开类型参数）。  
当前 C 前端**不会**构造带模板的诊断（占位实现），开关主要用于：

- 配合 `C++` 路径输出（`--target c++` 试验性）
- IDE 集成测试时验证开关被接受而不破坏现有诊断布局

`-fno-diagnostics-show-template-tree` 显式关闭（默认）。  
模板树解析为 O(N) 字符级扫描（`split_top_level_commas`），不依赖正则。

---

## 2. 编号规则

| 前缀 | 范围 | 子系统 | 源文件 |
|------|------|--------|--------|
| `E00xx` | 0001–0015 | Parser（语法/声明） | `src/parser.rs` |
| `W00xx` | 0001–0007 | Parser 警告 | `src/parser.rs`, `src/format_check.rs` |
| `E01xx` | 0100–0102 | Driver（I/O、汇编） | `src/driver.rs` |
| `E02xx` | 0200–0209 | Preprocessor（`#` 指令） | `src/preproc.rs` |
| `W02xx` | 0200 | Preprocessor 警告 | `src/preproc.rs` |
| `E03xx` | 0300–0310 | 目标/代码生成/链接 | `src/driver.rs`, `src/link/mod.rs` |
| `E04xx` | 0401 | PCH（预编译头） | `src/preproc.rs` |
| `E10xx` | 1001–1010 | Typed IR（语义/降级） | `src/ir.rs` |
| `W03xx` | 0301–0313 | 类型转换/OpenMP 警告 | `src/parser.rs`, `src/driver.rs` |
| `W04xx` | 0400–0403 | PCH 警告 | `src/preproc.rs` |
| `W10xx` | 1000 | 隐式声明警告 | `src/parser.rs` |

**严重性**：

- `E*`：硬错误，编译/链接中止（exit 1）
- `W*`：警告，默认不中止；Driver 在解析成功后仍会打印 warning（`-fsyntax-only` 亦如此）

---

## 3. Parser：`E0001`–`E0015`

语法分析、声明与表达式层面的错误。

| 码 | 消息（典型） | 含义 | 触发条件 | hint（如有） | Snapshot |
|----|--------------|------|----------|--------------|----------|
| **E0001** | `expected …, got …` | 期望特定 token | `expect()` 失败；asm/_Alignof 缺 `(`；`_Generic` 无匹配；`&&` 后非标签 | `check the syntax near this position` 等 | `e0001_alignof.c` |
| **E0002** | `expected ';', '=', '(', got …` | 声明不完整 | 变量/声明符后遇到非法 token | `declaration must end with ';' or have an initializer` | `e0002.c` |
| **E0003** | `expected struct name or '{'` / `expected type, got …` | 类型说明符非法 | `struct;`、说明符后非法 token | struct/typedef 相关 hint | `e0003.c` |
| **E0004** | `expected identifier, got …` | 需要标识符 | `int 123;`、typedef 名非法 | `identifiers must start with a letter or '_'` | `e0004.c` |
| **E0005** | `duplicate typedef '…'` / `duplicate enum member '…'` | 重复定义 | 同名 typedef 二次定义；enum 内重复成员名 | `each typedef name may only be defined once` 等 | `e0005.c` |
| **E0006** | `expected expression, got …` | 期望表达式 | `int x = ;`、不完整表达式 | `expected a value, variable, or expression here` | `e0006.c`, `bad.c` |
| **E0007** | `conflicting types for redeclaration of '…'` | 重复声明类型冲突 | 同作用域 `int x;` 后再 `float x;` | `use the same type as the previous declaration` | `e0007.c` |
| **E0008** | `variable declared with void type` | void 不能作对象类型 | `void v;` | `use 'void *' for generic pointers` | `e0008.c` |
| **E0009** | `unknown type or typedef '…'` | 未知类型名 | 未声明的 typedef/类型 ident | `declare the typedef before using it` | `e0009.c` |
| **E0010** | `_Static_assert` 消息字符串 | 静态断言失败 | `_Static_assert(0, "msg")` 条件为 0 | — | `e0010.c` |
| **E0011** | `division by zero…` / `not an integer constant expression` | 静态断言表达式非法 | `_Static_assert(1/0, …)` 等 | — | `e0011.c` |
| **E0012** | `expected string literal in _Static_assert` | 断言消息非字符串 | `_Static_assert(1, 123)` | `the second argument must be a diagnostic string` | `e0012.c` |
| **E0013** | `__builtin_choose_expr condition is not constant` | 选择表达式非常量 | 非常量条件传给 `__builtin_choose_expr` | — | `e0013.c` |
| **E0014** | `output operand N constraint '…' missing '=' or '+' prefix` | 输出约束缺前缀 | asm 输出操作数约束未以 `=` 或 `+` 开头 | `add '=' for write-only or '+' for read-write output` | `e0014.c` |
| **E0015** | `asm template references operand %N but only M operand(s) provided` / `asm template references unnamed operand '%[name]' which does not exist` | 模板占位符越界 | asm 模板引用的数字索引超出操作数范围，或命名操作数不存在 | `add the missing operand or fix the template index` | — |

### Parser 警告：`W0001`、`W0005`–`W0007`

| 码 | 消息 | 含义 | 触发条件 | Snapshot |
|----|------|------|----------|----------|
| **W0001** | `type defaults to 'int' in declaration without a type specifier` | 隐式 int（GNU 兼容） | 仅有存储类：`static x;` | `w0001.c` |
| **W0005** | `impossible constraint: 'r' constraint with floating-point operand` | 约束与类型不匹配 | asm 约束字母与操作数表达式类型不兼容（如浮点绑 `r`、整数绑 `x`） | `use 'x' for SSE or 'f' for x87 float operands` | — |
| **W0006** | `duplicate asm operand name '…'` | 重复操作数名 | asm 语句中多个操作数使用相同的 `[name]` | `each named operand must have a unique name` | — |
| **W0007** | `mixing named and unnamed asm operands` | 命名/位置混用 | asm 语句中部分操作数有 `[name]` 部分没有 | `name all operands or use positional references consistently` | — |

---

## 4. Driver：`E0100`–`E0102`、`E0300`–`E0310`

驱动层：读源、写产物、调汇编器、目标选项、代码生成后端。

| 码 | 消息（典型） | 含义 | 触发条件 | Snapshot |
|----|--------------|------|----------|----------|
| **E0100** | `no input file` / `cannot read …` | 输入缺失或不可读 | 无参数（Diagnostic 形式为后者）；打开失败 | `e0100_*` 测试 |
| **E0101** | `cannot write …` | 写文件失败 | `.o`/`.s`/`.d`/dep 文件写入失败 | `e0101.stderr.snap` |
| **E0102** | `cannot invoke assembler` / `assembler failed` | 汇编器失败 | 调 `as` 失败或非零退出 | `e0102.stderr.snap` |
| **E0300** | `floating-point code requires SSE…` | SSE 与浮点代码冲突 | `-mno-sse` 且 TU 含 float/double | `e0300.stderr.snap` |
| **E0301** | `dcc C backend failed for …` / `cannot write …` | 代码生成后端失败或写输出失败 | dcc C 后端非零退出；输出文件写入失败 | — |
| **E0310** | `a __global__ function '…' cannot also be __host__ or __device__` | CUDA 函数限定符冲突 | `__global__` 与 `__host__`/`__device__` 同时出现 | — |

**CLI 形式的 E0100**：无输入文件时，`cli.rs` 直接输出 `dcc: error: no input file`（无 `[E0100]`），exit 2。语义等价于 Diagnostic E0100。

---

## 5. Preprocessor：`E0200`–`E0209`、`E0401`、`W0200`、`W0400`–`W0403`

`#` 指令与翻译阶段 3 错误。  
**注意**：预处理错误在 Driver 中可能 **打印两次**（预处理阶段 + `main` 统一打印），snapshot 已锁定该行为。

| 码 | 消息（典型） | 含义 | 触发条件 | Snapshot |
|----|--------------|------|----------|----------|
| **E0200** | `#elif after #else` / `#elifdef after #else` / `#elifndef after #else` | 条件块结构错误 | `#else` 之后再次出现 `#elif*` | `e0200.c` |
| **E0201** | `duplicate #else` | 重复 `#else` | 同一 `#if` 链中两个 `#else` | `e0201.c` |
| **E0202** | `#endif without #if` | 无匹配 `#if` 的 `#endif` | 孤立的 `#endif` | `e0202.c` |
| **E0203** | `#error …` | `#error` 指令 | 源码中 `#error msg` | `e0203.c` |
| **E0204** | `expected "file" or <file>` | `#include` 语法错误 | `#include BOGUS` 等非法路径 token | `e0204.c` |
| **E0205** | `cannot read …: Permission denied` | 头文件存在但不可读 | `#include "secret.h"` 且文件权限拒绝 | `e0205.c`（Unix） |
| **E0206** | `include file not found: …` | 头文件未找到 | `#include "missing.h"` | `e0206.c` |
| **E0207** | `unterminated block comment` / `#embed` 语法 | 注释未闭合 / `#embed` 参数非法 | `/*` 未闭合；`#embed` 非文件路径 | `e0207.c` |
| **E0208** | `source is not valid UTF-8` / `embed file not found` | 源编码 / embed 文件 | 非法 UTF-8 字节；`#embed` 找不到文件 | `e0208_utf8`、`e0208_embed.c` |
| **E0209** | `cannot read forced include …` | 强制包含不可读 | `-include secret.h` 且读失败 | `e0209.c`（Unix） |
| **E0401** | `failed to load PCH …` / `cannot read PCH file …` | PCH 加载失败 | 预编译头文件损坏或不可读 | — |

### Preprocessor 警告：`W0200`、`W0400`–`W0403`

| 码 | 消息 | 含义 | 触发条件 | Snapshot |
|----|------|------|----------|----------|
| **W0200** | `#warning …` | `#warning` 指令 | 源码中 `#warning msg` | — |
| **W0400** | `PCH … is incompatible with current macro state` | PCH 不兼容 | 预编译头与当前宏状态冲突 | — |
| **W0402** | `failed to write PCH …` | PCH 写入失败 | 磁盘满或权限问题 | — |
| **W0403** | `PCH … not created due to preprocessing errors` | PCH 未创建 | 预处理阶段有错误 | — |

---

## 6. Link（`-r`）：`E0301`–`E0305`

可重定位链接 / LTO 合并路径（`src/link/mod.rs`）。

| 码 | 消息（典型） | 含义 | 触发条件 |
|----|--------------|------|----------|
| **E0301** | `cannot read input bundle: …` | 输入束读取失败 | 链接输入文件解析失败 |
| **E0302** | `symbol resolution failed: …` | 符号解析错误 | 未定义符号或重复定义 |
| **E0303** | `layout failed: …` | 布局计算失败 | ELF 布局构建错误 |
| **E0304** | `relocation failed: …` | 重定位失败 | 应用重定位时出错 |
| **E0305** | `failed to parse vDSO '…': …` | vDSO 解析失败 | `--vdso=` 指定的 ELF 文件格式错误或不存在 |

**Legacy 链接器**（`src/link/legacy.rs`）仍使用 `E0200`–`E0203`：

| 码 | 消息（典型） | 含义 | 触发条件 |
|----|--------------|------|----------|
| **E0200** | `no input files for link` | 链接无输入 | `dcc -r` 无 `.o` 输入 |
| **E0201** | `cannot read …` / `cannot write …` | 读写失败 | 输入/输出文件操作失败 |
| **E0202** | `cannot invoke ld` / `ld -r failed` | `ld` 失败 | 未安装 binutils 或 `LD=` 路径无效 |
| **E0203** | `cannot write LTO temp` / `objcopy failed…` | LTO 节注入失败 | 写临时文件或 `objcopy --update-section` 失败 |

---

## 7. Typed IR：`E1001`–`E1010`

AST 降级到 Typed IR 及验证阶段的语义错误（通常需 `-c` 或完整编译，非 `-fsyntax-only`）。

`validate` 覆盖：二元/一元运算、赋值、条件表达式、向量聚合初值、向量/复数类型转换。

| 码 | 消息（典型） | 含义 | 触发条件 | Snapshot |
|----|--------------|------|----------|----------|
| **E1001** | `unresolved type '…' reached typed IR` | 未解析类型进入 IR | `__typeof__(orphan_type) x;` 等产生 `Type::Named` | `e1001.c` |
| **E1002** | `IR function has no name` | 函数名为空 | `int () { return 0; }` 无名函数定义 | `e1002.c` |
| **E1003** | `unsupported floating-point operation …` | 不支持的 float 运算 | 如 `float` 的 `%`、一元 `!`/`~`；支持 `+ - * /` 比较与 `&& \|\|` | `e1003.c` |
| **E1004** | `cannot dereference a non-pointer expression` | 非法解引用 | `*x` 其中 `x` 非指针 | `e1004.c` |
| **E1005** | `va_arg currently requires a va_list object` | `va_arg` operand 非法 | 对非 `va_list` 对象做 `va_arg` | `e1005.c` |
| **E1006** | `global initializer for '…' is not a supported constant expression` | 全局初始化非常量 | `int g = main();`；支持整数/浮点字面量、`sizeof`/`offsetof`、cast、复合字面量、向量 `{1,2,3,4}` 等 | `e1006.c` |
| **E1007** | `unsupported vector operation …` / `vector assignment …` | 向量运算/赋值不支持 | 向量仅支持标量逐元素 `+ - *`（非 XMM SIMD）；赋值右值须为向量表达式、InitList 或 cast | `e1007.c`、`vector_binop_non_ident.c` |
| **E1008** | `unsupported complex operation …` | 复数运算不支持 | `_Complex float` 的 `%` 等 | `e1008.c` |
| **E1009** | `unknown aggregate member '…'` | 未知成员 | 结构体上访问不存在的字段 | `e1009.c` |
| **E1010** | `member access requires a struct or union` | 非聚合成员访问 | 对非 struct/union 做 `.` / `->` | `e1010.c` |

---

## 8. 警告码汇总

### Parser 警告：`W0001`–`W0004`、`W1000`

| 码 | 消息 | 含义 | 触发条件 | Snapshot |
|----|------|------|----------|----------|
| **W0001** | `type defaults to 'int' in declaration without a type specifier` | 隐式 int（GNU 兼容） | 仅有存储类：`static x;` | `w0001.c` |
| **W0002** | `unknown type '…' treated as 'int' (-fpermissive)` | 未知类型降级为 int | `-fpermissive` 且遇到未声明 typedef | — |
| **W0003** | `format attribute on '…' expects format string at argument …` 等 | printf/scanf 格式检查 | `__attribute__((format(printf,...)))` 参数不匹配 | — |
| **W0004** | `asm goto references label '…' which is not in the label list` | asm 模板引用的标签不在 goto label 列表中 | `asm goto("jmp %l[missing]" : : : : real)` | `w0004.c` |
| **W1000** | `implicit declaration of '…' (assumed int)` | 隐式函数声明 | 调用未声明的函数 | — |

### 类型转换警告：`W0301`–`W0302`

| 码 | 消息 | 含义 | 触发条件 |
|----|------|------|----------|
| **W0301** | `integer conversion from … to … bits may lose information` | 整数转换可能丢失信息 | 大类型赋值给小类型（如 `long` → `int`） |
| **W0302** | `implicit conversion from … (use an explicit cast if intended)` | 隐式类型转换 | 需要显式 cast 的隐式转换 |

### OpenMP 警告：`W0311`–`W0313`

| 码 | 消息 | 含义 | 触发条件 |
|----|------|------|----------|
| **W0311** | `-fopenmp: '#pragma omp …' outside any function body; ignored` | 函数体外 OpenMP 指令 | `#pragma omp` 出现在全局作用域 |
| **W0312** | `-fopenmp: unsupported OpenMP construct '#pragma omp …' in function` | 函数内不支持的 OpenMP | 函数体内遇到不支持的 OpenMP 构造 |
| **W0313** | `-fopenmp: unsupported OpenMP construct '#pragma omp …'; ignored` | 不支持的 OpenMP 指令 | 一般性不支持的 OpenMP 构造 |

---

## 9. CLI 非编号错误

`src/cli.rs` 中 `CliError`，格式固定为：

```text
dcc: error: <message>
```

常见消息：

| 消息 | 原因 |
|------|------|
| `no input file` | 非 `-E` 模式且无输入文件 |
| `unknown flag: …` | 无法识别的命令行选项 |
| `missing arg after …` | 选项缺少参数（`-o`、`-I`、`-include` 等） |
| `link mode (-r) cannot combine with …` | `-r` 与 `-S`/`-E`/`-fsyntax-only` 冲突 |
| `multiple source files cannot produce a single executable…` | 多 `.c` 直链 `a.out`；须 `-c` 后用 gcc 或 `dcc -r` |
| `multiple input files require -r link mode…` | 多输入既非全 `.c` 也非 `-r` 合并 `.o` |
| `relocatable link (-r) requires object (.o) inputs…` | `-r` 误用于 `.c` 源文件 |
| `unsupported -x language: …` | 不支持的语言 |

退出码：**2**（与 Diagnostic 错误的 exit 1 区分）。

---

## 10. Snapshot 回归

| 项目 | 说明 |
|------|------|
| 测试入口 | `cargo test --test errors --release` |
| Fixture | `tests/fixtures/errors/<name>.c` |
| 黄金文件 | `tests/fixtures/errors/<name>.stderr.snap` |
| 路径归一化 | 绝对路径 → `<file>`；`secret.h` → `<secret.h>` |
| 更新命令 | `DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors --release` |
| 脚本 | `bash scripts/update_error_snapshots.sh`（Linux） |
| CI | `.github/workflows/e2e.yml` 中 `cargo test --release` |

**平台限制**：

- `e0205` / `e0209`：依赖 Unix 文件权限（`chmod 000`），Windows 上跳过
- `e0208_utf8`：运行时写入含 `0xFF` 的临时源文件

**当前覆盖**：37 个 `.stderr.snap`（含 `diagnostic_format`、`w0001`、`e0208_utf8`、`e0205`、`e0209`、`e1009`、`e1010` 等）；未覆盖码见上表「Snapshot」列为 — 的条目。  
**新增覆盖**（2026-07-21）：`e0101.stderr.snap`、`e0102.stderr.snap`、`e0300.stderr.snap`、`e0205.stderr.snap`、`e0209.stderr.snap`。

**诊断选项回归**（与上述 snapshot 互补，验证 `-fdiagnostics-*` CLI 行为）：

| 测试 | 覆盖点 |
|------|--------|
| `diag_color_never_strips_ansi` | `-fdiagnostics-color=never` 不输出 `\x1b[` |
| `diag_color_always_emits_ansi`  | `-fdiagnostics-color=always` 必输出 `\x1b[` |
| `diag_color_rejects_unknown_value` | 非法 color 取值 → `dcc: error:` + 退出码 2 |
| `diag_format_clang_strips_brackets` | clang 模式首行 `error: …`，无 `[Exxxx]` |
| `diag_format_msvc_uses_file_line_col` | msvc 模式 `file(line,col): error Exxxx: …` |
| `diag_format_vi_is_single_line` | vi 模式只输出 1 行 |
| `diag_format_sarif_emits_json` | sarif 模式输出 `version: 2.1.0` + `name: dcc` + `ruleId` |
| `diag_format_rejects_unknown_value` | 非法 format 取值 → `dcc: error:` + 退出码 2 |
| `diag_template_tree_runs_without_panic` | `-fdiagnostics-show-template-tree` 不破坏现有诊断 |

**格式锁定**：

| 项目 | 说明 |
|------|------|
| 版本常量 | `tests/errors.rs` 中 `DIAGNOSTIC_FORMAT_VERSION`（当前 **1**） |
| 结构校验 | `validate_diagnostic_snapshot()`：首行码、` --> <file>:`、gutter、caret |
| 黄金样例 | `diagnostic_format.stderr.snap`（E0004 canonical 格式） |
| 全量 schema | `all_snapshots_match_locked_schema` 扫描全部 `.snap` |

---

## 11. 添加新错误码 checklist

1. 在对应子系统 `src/*.rs` 中使用 `Diagnostic::err` / `warn` 或 `err_here` / `warn_here`
2. 选用未占用编号（参见 §2 号段表，注意 Legacy Link 仍用 `E02xx`）
3. 在 `tests/fixtures/errors/` 添加最小触发 `.c` + 运行 snapshot 更新
4. **更新本文档**对应表格行
5. 若用户可见频率高，在 `docs/architecture.md` 保留一行链接即可

---

## 相关文档

- [architecture.md](./architecture.md) — 诊断架构与 M9 里程碑
- [ci_guide.md](./ci_guide.md) — CI 中 snapshot 失败处理
- [roadmap.md](./roadmap.md) — M9 精准错误诊断路线
