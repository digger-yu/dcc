# DCC 错误诊断码参考

> 最后更新：2026-07-17  
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

---

## 2. 编号规则

| 前缀 | 范围 | 子系统 | 源文件 |
|------|------|--------|--------|
| `E00xx` | 0001–0013 | Parser（语法/声明） | `src/parser.rs` |
| `W00xx` | 0001 | Parser 警告 | `src/parser.rs` |
| `E01xx` | 0100–0102 | Driver（I/O、汇编） | `src/driver.rs` |
| `E02xx` | 0200–0209 | Preprocessor（`#` 指令） | `src/preproc.rs` |
| `E02xx` | 0200–0204 | **Link（`-r` 链接）** ⚠️ 与预处理共用号段 | `src/link.rs` |
| `W02xx` | 0200 | Preprocessor 警告 | `src/preproc.rs` |
| `E03xx` | 0300 | 目标/代码生成选项 | `src/driver.rs` |
| `E10xx` | 1001–1010 | Typed IR（语义/降级） | `src/ir.rs` |

**⚠️ 号段重叠说明**：`E0200`–`E0204` 在 **预处理** 与 **链接** 中含义不同，需结合触发阶段（是否在 `-r` 链接路径）解读。后续版本可能将 Link 迁至 `E04xx`。

**严重性**：

- `E*`：硬错误，编译/链接中止（exit 1）
- `W*`：警告，默认不中止；Driver 在解析成功后仍会打印 warning（`-fsyntax-only` 亦如此）

---

## 3. Parser：`E0001`–`E0013`

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

### Parser 警告：`W0001`

| 码 | 消息 | 含义 | 触发条件 | Snapshot |
|----|------|------|----------|----------|
| **W0001** | `type defaults to 'int' in declaration without a type specifier` | 隐式 int（GNU 兼容） | 仅有存储类：`static x;` | `w0001.c` |

---

## 4. Driver：`E0100`–`E0102`、`E0300`

驱动层：读源、写产物、调汇编器、目标选项。

| 码 | 消息（典型） | 含义 | 触发条件 | Snapshot |
|----|--------------|------|----------|----------|
| **E0100** | `no input file` / `cannot read …` | 输入缺失或不可读 | 无参数（Diagnostic 形式为后者）；打开失败 | `e0100_*` 测试 |
| **E0101** | `cannot write …` | 写文件失败 | `.o`/`.s`/`.d`/dep 文件写入失败 | — |
| **E0102** | `cannot invoke assembler` / `assembler failed` | 汇编器失败 | 调 `as` 失败或非零退出 | — |
| **E0300** | `floating-point code requires SSE…` | SSE 与浮点代码冲突 | `-mno-sse` 且 TU 含 float/double | — |

**CLI 形式的 E0100**：无输入文件时，`cli.rs` 直接输出 `dcc: error: no input file`（无 `[E0100]`），exit 2。语义等价于 Diagnostic E0100。

---

## 5. Preprocessor：`E0200`–`E0209`、`W0200`

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

### Preprocessor 警告：`W0200`

| 码 | 消息 | 含义 | 触发条件 | Snapshot |
|----|------|------|----------|----------|
| **W0200** | `#warning …` | `#warning` 指令 | 源码中 `#warning msg` | — |

---

## 6. Link（`-r`）：`E0200`–`E0204`

可重定位链接 / LTO 合并路径（`src/link.rs`）。**与预处理同号不同义**。

| 码 | 消息（典型） | 含义 | 触发条件 |
|----|--------------|------|----------|
| **E0200** | `no input files for link` | 链接无输入 | `dcc -r` 无 `.o` 输入 |
| **E0201** | `cannot read …` | 无法读取 `.o` | 输入目标文件打开失败 |
| **E0202** | `cannot invoke ld` / `ld -r failed` | `ld` 失败 | 调 `ld -r` 失败或非零退出 |
| **E0203** | `cannot write LTO temp` / `objcopy failed…` | LTO 节注入失败 | 写临时文件或 `objcopy --update-section` 失败 |
| **E0204** | `cannot invoke objcopy` / `objcopy failed to strip…` | dead symbol 剥离失败 | LTO `--strip-symbol` 路径失败 |

---

## 7. Typed IR：`E1001`–`E1010`

AST 降级到 Typed IR 及验证阶段的语义错误（通常需 `-c` 或完整编译，非 `-fsyntax-only`）。

| 码 | 消息（典型） | 含义 | 触发条件 | Snapshot |
|----|--------------|------|----------|----------|
| **E1001** | `unresolved type '…' reached typed IR` | 未解析类型进入 IR | `__typeof__(orphan_type) x;` 等产生 `Type::Named` | `e1001.c` |
| **E1002** | `IR function has no name` | 函数名为空 | `int () { return 0; }` 无名函数定义 | `e1002.c` |
| **E1003** | `unsupported floating-point operation …` | 不支持的 float 运算 | 如 `float` 的 `%`、一元 `!`/`~`；支持 `+ - * /` 比较与 `&& \|\|` | `e1003.c` |
| **E1004** | `cannot dereference a non-pointer expression` | 非法解引用 | `*x` 其中 `x` 非指针 | `e1004.c` |
| **E1005** | `va_arg currently requires a va_list object` | `va_arg`  operand 非法 | 对非 `va_list` 对象做 `va_arg` | `e1005.c` |
| **E1006** | `global initializer for '…' is not a supported constant expression` | 全局初始化非常量 | `int g = main();`；支持整数/浮点字面量、`sizeof`/`offsetof`、cast、复合字面量、向量 `{1,2,3,4}` 等 | `e1006.c` |
| **E1007** | `unsupported vector operation …` | 向量运算不支持 | 向量类型上的 `\|`、一元运算等；支持 `+ - *` | `e1007.c` |
| **E1008** | `unsupported complex operation …` | 复数运算不支持 | `_Complex float` 的 `/` 等 | `e1008.c` |
| **E1009** | `unknown aggregate member '…'` | 未知成员 | 结构体上访问不存在的字段 | — |
| **E1010** | `member access requires a struct or union` | 非聚合成员访问 | 对非 struct/union 做 `.` / `->` | — |

---

## 8. CLI 非编号错误

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
| `multiple input files require -r link mode…` | 多输入未使用链接模式 |
| `unsupported -x language: …` | 不支持的语言 |

退出码：**2**（与 Diagnostic 错误的 exit 1 区分）。

---

## 9. Snapshot 回归

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

**当前覆盖**：35 个 `.stderr.snap`（含 `diagnostic_format`、`w0001`、`e0208_utf8`、`e0205`、`e0209` 等）；未覆盖码见上表「Snapshot」列为 — 的条目。

**格式锁定**：

| 项目 | 说明 |
|------|------|
| 版本常量 | `tests/errors.rs` 中 `DIAGNOSTIC_FORMAT_VERSION`（当前 **1**） |
| 结构校验 | `validate_diagnostic_snapshot()`：首行码、` --> <file>:`、gutter、caret |
| 黄金样例 | `diagnostic_format.stderr.snap`（E0004 canonical 格式） |
| 全量 schema | `all_snapshots_match_locked_schema` 扫描全部 `.snap` |

---

## 10. 添加新错误码 checklist

1. 在对应子系统 `src/*.rs` 中使用 `Diagnostic::err` / `warn` 或 `err_here` / `warn_here`
2. 选用未占用编号（避免与 Link/Preproc 的 `E02xx` 冲突，或更新本文档号段表）
3. 在 `tests/fixtures/errors/` 添加最小触发 `.c` + 运行 snapshot 更新
4. **更新本文档**对应表格行
5. 若用户可见频率高，在 `docs/ARCHITECTURE.md` 保留一行链接即可

---

## 相关文档

- [ARCHITECTURE.md](./ARCHITECTURE.md) — 诊断架构与 M9 里程碑
- [CI_GUIDE.md](./CI_GUIDE.md) — CI 中 snapshot 失败处理
- [ROADMAP.md](./ROADMAP.md) — M9 精准错误诊断路线
