# dcc

> **一个极简 C 编译器。比 GCC 更快，代码量超乎想象。**

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
| 自举 | 已实现 | 未实现 |
| 支持标准 | C89-C23 + GNU ext | **完全对标** |
| 目标架构 | 30+ | x86_64 已实现 / ARM64 / RISC-V / MIPS |
| 错误诊断 | 行级 | **字符级 + 修复建议** |
| 代码行数 | 700 万 | 目标 < 5 万 |
<!-- -->

### 核心承诺 — 已实现部分

| 承诺 | 当前状态 |
|------|----------|
| 编译 C 子集并生成可执行文件 | 已实现 — 支持整数运算、逻辑运算、控制流、函数调用、指针/数组、全局变量 |
| 字符级错误诊断 | 已实现 — 精确到行号+列号，源码引用，修复建议，彩色输出 |
| 手写 ELF64 输出 | 已实现 — ET_EXEC (可直接执行) + ET_REL (可重定位 .o) |
| 预处理器 | 基本实现 — #include/#define/#if 全家族 + 函数宏 + __has_include，但 __has_attribute 始终返回 0，__attribute__ 仅静默剥离 |
| 交叉编译框架 | 已实现 — TargetTriple 解析，多架构数据布局，后端分发 |
| CLI 兼容 GCC 参数 | 已实现 — -O0~O3, -std=c89~gnu23, --target, -ffreestanding, -I, -D, -c |
| 多文件编译 (.o 输出) | 已实现 — dcc -c 生成标准 .o，gcc 可链接，多文件链接验证通过 |
| 浮点运算 | 已实现 — float/double SSE 指令 + 类型转换 + 比较 |
| 位域/柔性数组/原子/SIMD/定点数 | 已实现 — 6 大后端模块全部通过 Linux 验证 |
| 编译期运算 (__builtin/attribute/typeof/alignof) | 已实现 — 9 大编译期模块全部实现+Linux 验证 |
<!-- -->

### 核心承诺 — 未实现部分

| 承诺 | 缺失内容 |
|------|----------|
| 编译 Linux 内核 | 内联汇编、函数指针、struct 整体传参等关键特性未实现 |
| ARM64 / RISC-V 后端 | 仅 x86_64 后端可用 |
| 自举 | 大量 C 特性未实现，无法编译自身源码 |
| 优化 pass | 预处理器 #if 常量求值已支持，编译器内部无独立优化 pass |
<!-- -->

## 里程碑

| 阶段 | 描述 | 状态 |
|------|------|------|
| M0 | 最小 hello world | 已完成 |
| M1 | C 子集 + 控制流 | 已完成 |
| M2 | struct/union/enum + 指针/数组 | 已完成 |
| M3 | 全局类型注册表 | 未开始 |
| M4 | 预处理 + builtin | 已完成 |
| M5 | 自举 (dcc 编 dcc) | 未开始 |
| M6 | Linux 内核 benchmark | 未开始 |
| M7 | ARM64 + RISC-V | 未开始 |
| M8 | C11/17/23 + GNU 扩展 | 已完成 |
| M9 | 精准错误诊断 | 已完成 |
| M10 | 跨平台交叉编译 | 未开始 |
<!-- -->

## 能力总表 — 按 Linux 内核需求优先级排序

### 一、基础设施

| <nobr>优先级</nobr> | <nobr>模块</nobr> | 功能 | 状态 | 说明 |
|--------|------|------|------|------|
| — | 前端 | 词法分析 | 已实现 | 完整 C token 集，关键字/数字/字符串/符号 |
| — | 前端 | 语法分析 | 已实现 | 递归下降 + Pratt 表达式解析 |
| — | 前端 | 类型系统 | 已实现 | void/char/short/int/long/long long/float/double + 指针/数组/struct/union/enum/typedef |
| — | 前端 | 预处理器 | 已实现 | #include/#define/#if 全家族 + 函数宏 + __has_include/attribute/builtin + GCC 兼容宏 |
| — | 前端 | 控制流 | 已实现 | if/else/while/for/do/goto/label + 三元运算符 |
| — | 前端 | 错误诊断 | 已实现 | 行号+列号 + 源码引用 + 修复建议 + 彩色输出 |
| — | 前端 | CLI | 已实现 | -O0~O3, -std=c89~gnu23, --target, -ffreestanding, -I, -D, -c |
| — | 前端 | 交叉编译框架 | 已实现 | TargetTriple 解析，多架构数据布局，后端分发 |
| — | 前端 | CI/CD | 已实现 | E2E 测试 11 项全通过 + Linux 内核编译 benchmark |
| P0 | 后端 | 可重定位 ELF (.o) | 已实现 | ET_REL + .text/.rodata/.data/.bss/.rela.text/.symtab，gcc 链接验证通过 |
| P0 | 后端 | 多文件编译 (-c) | 已实现 | dcc -c 输出标准 .o，符号表+重定位表完整 |
| — | 后端 | ELF 输出 (ET_EXEC) | 已实现 | 手写 ELF64 可直接执行 |
| — | 后端 | 字符串池 | 已实现 | .rodata 去重 + RIP-relative 寻址 |
<!-- -->

### 二、整数运算

| 优先级 | 模块 | 功能 | 状态 | 说明 |
|--------|------|------|------|------|
| — | 前端 | 整数算术 | 已实现 | 加减乘除 (+-*/)、取模 (%) |
| — | 前端 | 整数比较 | 已实现 | < <= > >= == != |
| — | 前端 | 位运算 | 已实现 | & | ^ ~ << >> |
| — | 前端 | 逻辑运算 | 已实现 | ! && || 短路求值，返回 0 或 1 |
| — | 前端 | 复合赋值 | 已实现 | += -= *= /= %= &= |= ^= <<= >>= |
| — | 前端 | 自增自减 | 已实现 | ++ -- (前缀/后缀) |
| — | 前端 | 整数类型转换 | 已实现 | 所有值以 64 位存储，隐式处理整数提升 |
| — | 前端 | typedef / enum | 已实现 | 完整支持，枚举常量内联替换 |
<!-- -->

### 三、指针与地址运算

| 优先级 | 模块 | 功能 | 状态 | 说明 |
|--------|------|------|------|------|
| P0 | 后端 | 指针±整数缩放 | 已实现 | ptr±n 按 sizeof(*ptr) 步进 |
| P0 | 后端 | 指针-指针 | 已实现 | 返回元素个数 |
| P1 | 后端 | 取地址 & | 已实现 | 获取变量/成员地址 |
| P1 | 后端 | 解引用 * | 已实现 | 访问指针所指内存 |
| P1 | 后端 | 指针比较 | 已实现 | < <= > >= == != 比较指针位置 |
| P1 | 后端 | 数组下标 [] | 已实现 | 本质是指针运算语法糖 |
| P2 | 后端 | void* 隐式转换 | 已实现 | 无类型检查，void* ↔ 任意指针隐式允许 |
| P2 | 后端 | 指针类型强制转换 | 已实现 | 语法支持，codegen 直接传递 (指针均为 64 位地址) |
<!-- -->

### 四、浮点与复数运算

| 优先级 | 模块 | 功能 | 状态 | 说明 |
|--------|------|------|------|------|
| P3 | 后端 | float 算术/比较 | 已实现 | 单精度 IEEE 754，SSE 指令 (addss/subss/mulss/divss) |
| P3 | 后端 | double 算术/比较 | 已实现 | 双精度 IEEE 754，SSE 指令 (addsd/subsd/mulsd/divsd) |
| P3 | 后端 | long double 支持 | 部分实现 | 8 字节存储，SSE 指令，未实现 x86 80 位扩展精度 |
| P3 | 后端 | 浮点字面量解析 | 已实现 | 1.5f / 2.0 / 3.0L / 1.5e10 等 |
| P3 | 后端 | 浮点类型转换 | 已实现 | cvtsi2sd/cvttsd2si/cvtss2sd/cvtsd2ss 等 SSE 指令 |
| P3 | 后端 | 浮点数学函数 | 已支持 | 由 libm 提供，函数调用已支持 |
| P3 | 后端 | _Complex 复数类型 | 未实现 | C99 复数，内核不使用 |
| P3 | 后端 | _Decimal 十进制浮点 | 未实现 | GCC 独有扩展，金融场景 |
<!-- -->

### 五、复合类型运算

| 优先级 | 模块 | 功能 | 状态 | 说明 |
|--------|------|------|------|------|
| P0 | 后端 | struct 成员访问 (.和->) | 已实现 | codegen 支持基址+偏移寻址，支持.和->操作符 |
| P0 | 后端 | switch/case/default | 已实现 | 比较链实现，支持default分支 |
| P1 | 后端 | struct 整体赋值 | 部分实现 | 支持成员级赋值，大struct memcpy未实现 |
| P1 | 后端 | 指定初始化器 | 已实现 | struct foo f = { .bar = 1 }; 支持字段和数组指定 |
| P1 | 前端 | 聚合类型初始化 | 已实现 | 花括号初始化列表 = { ... } 支持嵌套 |
| P1 | 后端 | 全局变量字符串初始化 | 已实现 | char s[] = "hello" 支持 |
| P2 | 后端 | 复合字面量 | 已实现 | (struct foo){1, 2} 支持 |
| P2 | 后端 | struct 传参/返回值 | 未实现 | SysV ABI 的寄存器/栈传递规则未实现 |
| P2 | 后端 | union 成员访问 | 已实现 | codegen 同 struct，偏移均为 0 |
| P2 | 后端 | 数组初始化 | 已实现 | int a[] = {1,2,3}; 支持 |
| P2 | 后端 | struct 初始化器 | 已实现 | struct 聚合初始化支持 |
| P2 | 后端 | 枚举比较/赋值 | 已实现 | 本质是整数运算 |
<!-- -->

### 六、GCC 扩展类型

| 优先级 | 模块 | 功能 | 状态 | 说明 |
|--------|------|------|------|------|
| P2 | 后端 | 位域 bitfields | 已实现 | 位偏移计算 + 掩码移位 + 有符号扩展 + 跨容器边界 |
| P2 | 后端 | 柔性数组成员 | 已实现 | sizeof 不贡献 + 偏移紧跟前一字段 |
| P3 | 后端 | 向量类型 (SIMD) | 已实现 | 标量逐元素加减乘，帧底临时区传递 |
| P3 | 后端 | 定点数类型 | 已实现 | _Fract(1.31) / _Accum(16.16) 复用整数路径 |
| P3 | 后端 | _Atomic 原子操作 | 已实现 | lock xadd / cmpxchg 循环 / lock xchg |
| P3 | 后端 | 原子内存序 | 已实现 | __ATOMIC_* 宏 + seq_cst=mfence |
<!-- -->

### 七、编译期运算

| <nobr>优先级</nobr> | <nobr>模块</nobr> | 功能 | 状态 | 说明 |
|--------|------|------|------|------|
| P1 | 后端 | __builtin__ 语义 | 已实现 | __builtin_expect/constant_p/memcpy/memset/abs/trap 等 codegen，__has_builtin 查询 |
| P1 | 后端 | __attribute__ 语义 | 已实现 | packed/aligned/noreturn/unused 等解析器静默剥离，__has_attribute 查询 |
| P2 | 后端 | sizeof(type) | 已实现 | 编译期返回类型大小 |
| P2 | 后端 | sizeof(expr) | 已实现 | 计算实际表达式类型大小 |
| P2 | 后端 | _Alignof 对齐查询 | 已实现 | C11 _Alignof(type)/_Alignof(expr) 返回对齐值 |
| P3 | 后端 | typeof 运算符 | 已实现 | GNU 扩展 typeof(expr/type) 推导类型，内核广泛使用 |
| P3 | 后端 | __builtin_constant_p | 已实现 | 判断是否为编译期常量 (当前返回 0) |
| P3 | 后端 | 常量表达式折叠 | 部分实现 | 预处理器 #if 支持，编译器内部无独立优化 pass |
| P3 | 后端 | __extension__ | 已实现 | GNU 扩展，解析器直接跳过抑制警告 |
| P3 | 后端 | 语句表达式 ({...}) | 已实现 | GNU 扩展，块内最后表达式值作为结果 |
<!-- -->

### 八、函数与调用

| 优先级 | 模块 | 功能 | 状态 | 说明 |
|--------|------|------|------|------|
| P0 | 后端 | 函数调用 (标识符) | 已实现 | call_label 直接调用，SysV ABI 寄存器传参 |
| P1 | 后端 | 函数指针调用 | 未实现 | Call 仅支持标识符，不支持表达式 callee |
| P2 | 后端 | 逗号运算符 | 未实现 | for(i=0,j=0; ...; i++,j++) 不支持 |
| P2 | 后端 | VLA 可变长数组 | 前端完成 | 解析+类型已实现，分配为 TODO |
| P2 | 后端 | 完整类型检查 | 未实现 | 无隐式整数提升、无类型兼容性验证 |
<!-- -->

### 九、GNU 扩展与特殊语法

| 优先级 | 模块 | 功能 | 状态 | 说明 |
|--------|------|------|------|------|
| P1 | 后端 | 内联汇编 __asm__ | 未实现 | 内核原子操作/上下文切换/中断处理强依赖 |
| P3 | 后端 | 向量类型运算 | 已实现 | 标量逐元素加减乘，不用 XMM 寄存器 |
| P3 | 后端 | 定点数运算 | 已实现 | _Fract/_Accum 加减，复用整数 codegen |
<!-- -->

### 十、C++ 专属 (不适用)

DCC 定位为 C 编译器，以下 C++ 专属能力不在当前路线图中:
- 运算符重载、Lambda 表达式、RTTI (typeid/dynamic_cast)
- constexpr/consteval、模板元编程
- 类/继承/虚函数、命名空间
- std::complex、std::atomic

### Linux 内核编译硬性门槛

```
必须全部满足才能开始真正的内核编译:
  ☑ 可重定位 ELF (.o 输出)     ← 已完成: ET_REL + 符号表 + 重定位，gcc 链接通过
  ☑ 多文件编译 (-c)            ← 已完成: dcc -c 输出标准 .o
  ☑ 指针运算缩放               ← 已完成: ptr±n 按 sizeof(*ptr) 缩放
  ☑ 取地址 & / 解引用 *        ← 已完成: 基本指针操作可用
  ☑ struct 成员读写            ← 已完成: codegen 支持基址+偏移寻址
  ☑ switch/case                ← 已完成: 比较链实现，支持 default 分支
  ☑ 指定初始化器               ← 已完成: .field = expr 语法
  ☑ 位域/柔性数组/原子/SIMD/定点数 ← 已完成: 6 大后端模块全部实现
  ☑ __builtin_expect/constant_p/memcpy ← 已完成: codegen 层内建函数处理
  ☑ __attribute__ 语义         ← 已完成: packed/aligned/noreturn/unused 解析+剥离
  ☑ sizeof(expr)/_Alignof/typeof/__extension__/语句表达式 ← 已完成
  □ __asm__ 内联汇编透传       ← 当前: 不支持
  □ 函数指针                   ← 当前: 不支持
  □ struct 整体传参/返回       ← 当前: 不支持
```

> **注**: `kernel-bench.yml` 是编译性能基准测试（测量编译速度），不是真正编译内核。

## 哲学 (suckless)

> "能用 50 行代码解决的不写 200 行。"

每行代码必须值回它的存在成本。参考 dwm/suckless 风格:
- 零冗余文件 IO
- 不引入不必要的抽象层
- AST 即 IR，不做多余的中间表示转换
- 直接 emit 机器码字节，不依赖外部汇编器/链接器

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

# 生成可重定位目标文件 (.o)
dcc -c input.c -o input.o

# 多文件链接 (通过 gcc)
dcc -c a.c -o a.o
dcc -c b.c -o b.o
gcc a.o b.o -o program

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
    └── elf.rs       # ELF64 生成器: ET_EXEC + ET_REL
```

## CI/CD

### E2E 测试
每次 push/PR 自动验证基础测试用例:
- `hello.c` → exit 42
- `arith.c` → exit 23
- `control.c` → exit 100
- `call.c` → exit 42
- `codegen_test.c` → exit 42
- `pp_simple_test.c` → exit 42
- `pp_attr_test.c` → exit 30
- `call_assign_test.c` → exit 20
- `bar_test.c` → exit 20
- `ptr_arith_test.c` → exit 64
- `fib.c` → exit 55

### 可重定位 ELF 测试
- 单文件 .o 生成 + gcc 链接 → exit 0
- 多文件 .o 生成 + gcc 链接 → exit 42

### 内核编译 Benchmark
每周一 + 手动触发，在 GitHub Actions 上编译 Linux 内核:
- GCC 基线对比
- 自动生成 Markdown 性能报告
- 支持自定义内核版本/优化级别/并行数

## 技术决策 (ADR)

| 决策 | 选择 | 理由 |
|------|------|------|
| 语言 | Rust | 内存安全 + 零成本抽象 + 现代工具链 |
| 表达式解析 | Pratt parsing | 比递归下降更简洁，优先级驱动 |
| 帧指针 | R13 | 释放 RBP，简化栈帧管理 |
| ELF | 手写 | 避免依赖 ld/binutils，完全控制输出格式 |
| 字符串存储 | .rodata 池 | 去重 + RIP-relative 寻址 |
| 函数调用重定位 | R_X86_64_PLT32 | 兼容 gcc PIE 默认设置 |
| 数据引用重定位 | R_X86_64_PC32 | 小代码模型下的 RIP-relative 寻址 |
| 错误格式 | GCC 风格 | 开发者熟悉，工具链兼容 |
<!-- -->

## License

MIT
