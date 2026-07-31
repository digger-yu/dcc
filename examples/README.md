# DCC 示例程序

本目录按主题分目录组织，每个文件展示 DCC 编译器支持的特定特性。

## 目录结构

```
examples/
├── basics/          # 入门级 — 基本语法与控制流
├── pointers/        # 指针与函数调用
├── structs/         # 结构体、联合体、位域、数组初始化
├── floats/          # 浮点运算与类型转换
├── preprocessor/    # 预处理器指令与宏
├── assembly/        # 内联汇编 (GNU asm)
├── builtins/        # 编译器内建函数 (__builtin_*)
├── cpp/             # C++ 示例
└── advanced/        # 进阶特性: VLA, atomic, SIMD, __int128, ELF 属性等
```

## 快速入门

从 `basics/` 开始，按顺序阅读：

| 文件 | 演示内容 |
|------|----------|
| `basics/hello.c` | 最简单的程序入口与 exit code |
| `basics/arith.c` | 四则运算与表达式求值 |
| `basics/fib.c` | 递归函数调用 |
| `basics/logic.c` | 逻辑运算 (`&&`, `||`, `!`) |
| `basics/control_flow.c` | 条件分支与循环 |
| `basics/switch.c` | switch-case 多路分支 |
| `basics/compound_assign.c` | 复合赋值操作 (`+=`, `-=` 等) |

## 编译运行

```bash
# 编译并运行
dcc examples/basics/hello.c -o /tmp/hello && /tmp/hello

# 仅语法检查
dcc -fsyntax-only examples/basics/hello.c

# 编译为目标文件
dcc -c -o /tmp/hello.o examples/basics/hello.c
```