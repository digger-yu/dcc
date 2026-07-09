# dcc

> **A suckless C compiler. Faster than GCC. Less code than you think.**

## 第一性原理

```
GCC  =  词法  +  语法  +  语义  +  IR  +  优化  +  代码生成
DCC  =  AST(同IR)  +  共享类型  +  零冗余文件IO  +  函数级并行
```

## 核心承诺

| 指标 | GCC | DCC 目标 |
|------|-----|----------|
| 编译 linux 6.6 内核 | 30-60 分钟 | **< 30 分钟** |
| 自举 | ✅ | ✅ (用 dcc 编 dcc) |
| 支持标准 | C89-C23 + GNU ext | **完全对标** |
| 目标架构 | 30+ | x86_64(已) / ARM64(计划) / RISC-V(计划) / MIPS(计划) |
| 错误诊断 | 行 | **字符 + 修复建议** |
| LOC | 700 万 | 目标 < 5 万 |

## 路线图

- [x] M0  最小 hello world  (本次)
- [ ] M1  C89/99 子集 + 控制流
- [ ] M2  struct/union/enum + 指针
- [ ] M3  全局类型注册表
- [ ] M4  宏 + builtin
- [ ] M5  **自举**
- [ ] M6  Linux 内核 benchmark
- [ ] M7  ARM64 / RISC-V
- [ ] M8  C11/17/23 + GNU 扩展
- [ ] M9  精准错误诊断
- [ ] M10 跨平台交叉编译

## 哲学 (suckless)

> "能用 50 行代码解决的不写 200 行。"

每行代码必须值回它的存在成本。

## 构建

```bash
cargo build --release
./target/release/dcc examples/hello.c
chmod +x a.out && ./a.out
echo $?   # 期望 42
```
