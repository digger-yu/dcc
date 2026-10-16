# DCC 冒烟回归语料（smoke）

本目录是 DCC 的**端到端冒烟回归语料**：每个文件都是完整、可编译、可运行的 C/C++ 程序，
由 `scripts/test_all.sh`、CI 的 `ci.yml` / `kernel-bench.yml` 逐个「编译 + 运行 + 校验退出码」。
它们与 `tests/fixtures/c/`（单特性、喂进 harness 的夹具）不同——这里强调的是「跑通整程序、验退出码」。

> 迁移说明：本目录前身是仓库顶层 `examples/`（按主题分目录的示例程序）。为收敛测试相关路径、
> 保持根目录简洁，已扁平并入 `tests/fixtures/smoke/`，仅保留被引用到的用例。

## 文件清单（26 个）

| 文件 | 演示内容 |
|------|----------|
| `hello.c` | 最简单的程序入口与 exit code |
| `arith.c` | 四则运算与表达式求值 |
| `fib.c` | 递归函数调用 |
| `logic.c` | 逻辑运算 (`&&`, `\|\|`, `!`) |
| `control_flow.c` | 条件分支与循环 |
| `switch.c` | switch-case 多路分支 |
| `compound_assign.c` | 复合赋值操作 (`+=`, `-=` 等) |
| `call_by_assign.c` / `bar_call.c` / `function_call.c` | 函数调用与求值栈 |
| `pointer_arith.c` / `rel_ops.c` | 指针运算与关系运算 |
| `basic_struct.c` / `bitfield.c` / `global_array.c` | 结构体、位域、全局数组 |
| `float_cast.c` | 浮点类型转换 |
| `const_expr.c` / `attributes.c` / `attributes_simple.c` | 常量表达式与属性 |
| `constraints.c` | 内联 asm 约束 (`=r` / `+m` / memory clobber) |
| `atomic.c` / `nested_func.c` / `runtime_features.c` / `elf_attrs.c` / `va_list.c` | 进阶特性：原子、嵌套函数、运行期特性、ELF 属性、变参 |
| `bad_syntax.c` | 负向用例：验证错误诊断 (`error[E0006]` + `hint:`) |

## 编译运行

```bash
# 编译并运行
dcc tests/fixtures/smoke/hello.c -o /tmp/hello && /tmp/hello

# 仅语法检查
dcc -fsyntax-only tests/fixtures/smoke/hello.c

# 编译为目标文件
dcc -c -o /tmp/hello.o tests/fixtures/smoke/hello.c
```

## 与测试用例脚本 check_fixtures.sh 的关系

本目录的文件会被 `scripts/check_fixtures.sh` 纳入 `tests/fixtures/**` 的引用完整性检查：
每个 `.c/.cpp` 必须被某个 `.rs` / `.sh` / `.yml` 以字面量路径引用，否则判为 `UNREFERENCED`。
因此新增/删除用例时，务必同步其在 CI 或脚本中的引用点。
