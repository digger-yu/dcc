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
- [x] **M2** struct/union/enum + 指针/数组 (前端完成)
- [x] **M3** 预处理器 + 宏展开
- [x] **M4** 全局数据段 + 字符串字面量
- [x] **M5** 错误诊断增强 (源码引用 + 修复建议)
- [ ] **M6** 代码生成补全 — struct 成员访问、switch/case、VLA 栈分配
- [ ] **M7** 语义分析 — 类型检查、隐式转换、sizeof 精确化 (指针运算缩放已完成)
- [ ] **M8** C99/C11 特性 — 复合字面量、指定初始化、柔性数组、逗号运算符
- [ ] **M9** 可重定位 ELF — 输出 .o 文件、符号表、重定位段
- [ ] **M10** GNU 扩展 — `__asm__` 内联汇编、`__builtin_*` 内建函数、`__attribute__` 语义
- [ ] **M11** 浮点支持 — SSE/x87 指令生成、浮点字面量、类型转换
- [ ] **M12** ARM64 后端
- [ ] **M13** RISC-V 后端
- [ ] **M14** 优化 pass (常量折叠/死代码消除/内联)
- [ ] **M15** 自举 (用 dcc 编译 dcc)
- [ ] **M16** Linux 内核完整编译验证

### 能力缺口分析 — 对标 Linux 内核编译

> 编译 Linux 内核是 DCC 成功的标志。以下是当前实现与内核需求之间的精确差距。

#### 已完全实现 ✅

| 功能 | 说明 |
|------|------|
| 词法/语法分析 | 完整 C token 集 + 递归下降 Pratt 解析 |
| 预处理器 | `#include/#define/#if` 全家族 + 函数宏 + `__has_include/attribute/builtin` |
| 控制流 | if/else/while/for/do/goto/label + 三元运算符 |
| 基础类型 | void/char/short/int/long/long long + 指针 + 数组 |
| typedef / enum | 完整支持，枚举常量内联替换 |
| 复合赋值 | `+=` `-=` `*=` 等 10 种，展开为 `target = target op expr` |
| 指针运算缩放 | `ptr ± n` 按 `sizeof(*ptr)` 步进，`ptr++`/`ptr--` 同理，`ptr - ptr` 返回元素数 |
| 字符串池 | .rodata 去重 + RIP-relative 寻址 |
| 全局变量 | 整数初始化 + 零填充 |
| ELF 输出 | 手写 ELF64 ET_EXEC + .text/.rodata |
| 错误诊断 | 行号+列号 + 源码引用 + 修复建议 + 彩色输出 |

#### 前端完成 / 后端待补 🔧

| 功能 | 前端状态 | 后端缺失 |
|------|----------|----------|
| **struct/union 成员访问** | 解析 + 类型布局 ✅ | `Expr::Member` 无 codegen，struct 变量无法读写 |
| **switch/case/default** | 完整解析 ✅ | codegen 为空 `/* TODO */` |
| **VLA 可变长数组** | AST + 解析 ✅ | `gen_vla_alloc()` 为空 TODO，固定帧与 VLA 冲突待解决 |
| **位域 (bitfields)** | `bit_width` 字段已解析 ✅ | 无位偏移计算、无掩码/移位代码生成 |

#### 部分实现 ⚠️

| 功能 | 已有 | 缺失 |
|------|------|------|
| **void*** | 类型可声明 ✅ | 无隐式转换逻辑 |
| **sizeof** | `sizeof(type)` ✅ | `sizeof expr` 硬编码返回 8，未实际求类型大小 |
| **类型转换 (cast)** | `(type)expr` 语法 ✅ | 无截断/扩展/浮点转换指令，仅改类型标注 |
| **`__attribute__`** | 预处理层静默剥离 ✅ | 无语义实现（`inline`/`packed`/`aligned` 等不生效） |
| **`__builtin_*`** | `__has_builtin` 返回 1 ✅ | 调用 builtin 会编译失败，无实际实现 |
| **全局变量初始化** | 整数初始值 ✅ | 不支持字符串/数组/struct 初始化器 |

#### 完全缺失 ❌ — 按 Linux 内核需求优先级排序

| 优先级 | 功能 | Linux 内核使用频率 | 说明 |
|--------|------|-------------------|------|
| **P0** | **可重定位 ELF (.o)** | 每次编译 | 内核是数百万个 .o 最终链接；当前只能输出 ET_EXEC |
| **P0** | **struct 成员访问 codegen** | 极高频 | 内核大量 struct list_head / task_struct 等 |
| **P0** | **switch/case codegen** | 高频 | 系统调用分发、状态机等 |
| ~~**P0**~~ | ~~**指针运算缩放**~~ | ~~极高频~~ | ✅ 已实现：`ptr++` 按 `sizeof(*ptr)` 步进，`ptr + n` 缩放，`ptr - ptr` 返回元素数 |
| **P1** | **内联汇编 `__asm__`** | 高频 | 原子操作、上下文切换、中断处理依赖大量 asm |
| **P1** | **`__builtin_*` 实现** | 高频 | `__builtin_expect`/`__builtin_memcpy`/`__builtin_offsetof` 等 |
| **P1** | **`__attribute__` 语义** | 高频 | `packed`/`aligned`/`noreturn`/`unused` 影响布局和警告 |
| **P1** | **指定初始化器** | 高频 | `struct foo f = { .bar = 1, .baz = 2 };` 内核惯用模式 |
| **P1** | **复合字面量** | 中频 | `(struct sockaddr_in){ .sin_family = AF_INET }` |
| **P2** | **浮点指令生成** | 低频 (内核不用) | SSE/x87 指令、浮点字面量解析 |
| **P2** | **`_Atomic` / 内存序** | 中频 | C11 原子操作，内核部分使用 |
| **P2** | **逗号运算符** | 中频 | `for(i=0,j=0; ...; i++,j++)` |
| **P2** | **函数指针** | 高频 | 内核回调/虚表机制；当前 Call 仅支持标识符 |
| **P2** | **柔性数组成员** | 中频 | `struct { int len; char data[]; }` 内核网络/文件系统常用 |
| **P3** | **完整类型检查** | — | 隐式转换规则、类型兼容性、警告 |

#### Linux 内核编译的硬性门槛

```
必须全部满足才能开始真正的内核编译:
  □ 可重定位 ELF (.o 输出)     ← 当前: 仅 ET_EXEC
  □ struct 成员读写            ← 当前: 前端完成,后端空
  □ switch/case                ← 当前: 前端完成,后端空
  ☑ 指针运算缩放               ← 已完成: ptr±n 按 sizeof(*ptr) 缩放, ptr-ptr 返回元素数
  □ __asm__ 内联汇编透传       ← 当前: 不支持
  □ __builtin_expect           ← 当前: 不支持 (likely/unlikely 宏依赖)
  □ __attribute__ 语义         ← 当前: 仅预处理剥离
  □ 指定初始化器               ← 当前: 不支持
```

> **注**: `kernel-bench.yml` 是编译性能基准测试（测量编译速度），不是真正编译内核。

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
