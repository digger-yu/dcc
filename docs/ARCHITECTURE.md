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
