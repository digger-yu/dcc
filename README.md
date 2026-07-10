# dcc

> **A suckless C compiler. Faster than GCC. Less code than you think.**

## 第一性原理

```
编译器的本质 = 将人类可读的文本无损转换为机器可执行的二进制指令
GCC 的本质 = 多语言 + 多目标平台的编译器基础设施
DCC 的目标 = GCC 能干的，DCC 都能干，而且干的更好
```

## 核心承诺

| 指标 | GCC | DCC 目标 |
|------|-----|----------|
| 编译 linux 6.6 内核 | 30-60 分钟 | **< 30 分钟** |
| 自举 | ✅ | ✅ (用 dcc 编 dcc) |
| 支持标准 | C89-C23 + GNU ext | **完全对标** |
| 目标架构 | 30+ | x86_64 ✅ / ARM64 / RISC-V / MIPS |
| 错误诊断 | 行 | **字符 + 修复建议** |
| LOC | 700 万 | 目标 < 5 万 |

## 当前进度

### 已完成 ✅

| 模块 | 状态 | 说明 |
|------|------|------|
| **词法分析** | ✅ | 完整 C token 集, 关键字/数字/字符串/符号 |
| **语法分析** | ✅ | 递归下降 + Pratt 表达式解析 |
| **类型系统** | ✅ | void/char/short/int/long/long long/float/double + 指针/数组/struct/union/enum/typedef |
| **预处理器** | ✅ | #include/#define/#undef/#ifdef/#ifndef/#if/#elif/#else/#endif + 函数宏 + 常量表达式 |
| **代码生成 x86_64** | ✅ | R13 帧指针, SysV ABI, if/while/for/do-while/goto/label/switch |
| **ELF 生成** | ✅ | 手写 ELF64, .text + .rodata 段, RIP-relative 字符串寻址 |
| **全局数据段** | ✅ | .rodata (字符串池 + 全局变量), 对齐 |
| **错误诊断** | ✅ | 精确到字符(行号+列号), 源码引用, 修复建议, 彩色输出 |
| **CLI** | ✅ | -O0~O3, -std=c89~gnu23, --target, -ffreestanding, -I, -D, 内核编译 flags 兼容 |
| **交叉编译框架** | ✅ | TargetTriple 解析, 多架构数据布局, 后端分发 |
| **CI/CD** | ✅ | E2E 测试 + Linux 内核编译 benchmark (GCC 对比) |

### 路线图

- [x] **M0** 最小 hello world — 能编译 `int main() { return 42; }`
- [x] **M1** C 子集 + 控制流 — 算术/比较/if/while/函数调用
- [x] **M2** struct/union/enum + 指针/数组
- [x] **M3** 预处理器 + 宏展开
- [x] **M4** 全局数据段 + 字符串字面量
- [x] **M5** 错误诊断增强 (源码引用 + 修复建议)
- [ ] **M6** 完善语义分析 (类型检查/隐式转换/作用域)
- [ ] **M7** ARM64 后端
- [ ] **M8** RISC-V 后端
- [ ] **M9** 优化 pass (常量折叠/死代码消除/内联)
- [ ] **M10** 自举 (用 dcc 编译 dcc)
- [ ] **M11** Linux 内核完整编译验证
- [ ] **M12** C11/C17/C23 完整支持 + GNU 扩展

## 哲学 (suckless)

> "能用 50 行代码解决的不写 200 行。"

每行代码必须值回它的存在成本。参考 dwm/suckless 风格:
- 零冗余文件 IO
- 不引入不必要的抽象层
- AST 即 IR, 不做多余的中间表示转换
- 直接 emit 机器码字节, 不依赖外部汇编器/链接器

## 构建

```bash
cargo build --release
./target/release/dcc examples/hello.c
chmod +x a.out && ./a.out
echo $?   # 期望 42
```

## 使用

```bash
# 基础编译
dcc input.c -o output

# 指定标准
dcc -std=gnu23 input.c
dcc -std=c11 input.c

# 交叉编译
dcc --target aarch64-linux-gnu input.c
dcc --target riscv64-linux-gnu input.c

# 独立环境 (内核编译)
dcc -ffreestanding -nostdinc kernel.c

# 预处理选项
dcc -I./include -DDEBUG=1 -DCONFIG_X input.c

# 调试
dcc -dump-ast input.c
dcc -v input.c
```

## 项目结构

```
src/
├── main.rs          # 入口: 参数 → 配置 → driver
├── cli.rs           # CLI 解析: GCC 兼容的命令行选项
├── driver.rs        # 驱动: 读 → 预处理 → 解析 → codegen → 写
├── preproc.rs       # 预处理器: #include/#define/#if/宏展开
├── tokens.rs        # 词法分析: 字符流 → token 流
├── parser.rs        # 语法分析: token 流 → AST (递归下降 + Pratt)
├── ast.rs           # AST 定义: Type/Expr/Stmt/Decl
├── diagnostics.rs   # 错误诊断: 精确到字符 + 源码引用 + 修复建议
├── target.rs        # 目标平台: 数据布局/调用约定/字节序
└── codegen/
    ├── mod.rs       # 后端调度: 按 target 选后端
    ├── x86_64.rs    # x86_64 代码生成: 直接 emit 字节
    └── elf.rs       # ELF64 生成器: 手写 header + program header
```

## CI/CD

### E2E 测试
每次 push/PR 自动验证基础测试用例:
- `hello.c` → exit 42
- `arith.c` → exit 23
- `control.c` → exit 100
- `call.c` → exit 42

### 内核编译 Benchmark
每周一 + 手动触发, 在 GitHub Actions 上编译 Linux 内核:
- GCC 基线对比
- 自动生成 Markdown 性能报告
- 支持自定义内核版本/优化级别/并行数

## 技术决策 (ADR)

| 决策 | 选择 | 理由 |
|------|------|------|
| 语言 | Rust | 内存安全 + 零成本抽象 + 现代工具链 |
| 表达式解析 | Pratt parsing | 比递归下降更简洁, 优先级驱动 |
| 帧指针 | R13 | 释放 RBP, 简化栈帧管理 |
| ELF | 手写 | 避免依赖 ld/binutils, 完全控制输出格式 |
| 字符串存储 | .rodata 池 | 去重 + RIP-relative 寻址 |
| 错误格式 | GCC 风格 | 开发者熟悉, 工具链兼容 |

## License

MIT
