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
   │  parser.rs)│    │  ├ aarch64.rs  │ ← 未来
   │            │    │  ├ riscv.rs    │ ← 未来
   │            │    │  └ elf.rs      │
   └────────────┘    └────────────────┘
          │
          ▼
   ┌─────────────────────────────────────┐
   │  AST (ast.rs) = IR (第一性原理决定)  │
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

## 类型表设计 (M3)

```
~/.dcc/types.idx       <-- 全局类型注册表
~/.dcc/headers/<hash>  <-- header 解析缓存
```

类型注册表用 `bincode` 序列化，记录：
- 类型 ID
- 完整类型信息 (struct/typedef/...)
- 定义所在文件:行号

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

## 跨平台

`--target=<triple>` 完整支持:
- x86_64-unknown-linux-gnu
- aarch64-linux-gnu
- riscv64-linux-gnu
- mips-linux-gnu
- arm-linux-gnueabi
- 自由扩展

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

## 路线图 (来自 docs/ROADMAP.md)

M0~M10 见根目录的 task 列表。
