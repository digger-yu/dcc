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

### Snapshot 回归（tests/errors.rs）

失败路径的诊断格式由 snapshot 锁定，与 59 项 integration 成功路径互补。  
**完整错误码说明见 [DIAGNOSTICS.md](./DIAGNOSTICS.md)。**

| 目录 | 内容 |
|------|------|
| `tests/fixtures/errors/*.c` | 触发 fixture |
| `tests/fixtures/errors/*.stderr.snap` | 归一化 stderr 黄金文件（35 个，含 `diagnostic_format`） |
| `scripts/update_error_snapshots.sh` | Linux 上一键刷新 snapshot |

`validate_diagnostic_snapshot()` 锁定 GitHub 四段式结构；变更
`diagnostics.rs` 布局时递增 `DIAGNOSTIC_FORMAT_VERSION` 并全量刷新。

更新 snapshot：`DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors`。

## 跨平台

`--target=<triple>` 完整支持:
- x86_64-unknown-linux-gnu
- aarch64-linux-gnu
- riscv64-linux-gnu
- mips-linux-gnu
- arm-linux-gnueabi
- 自由扩展

## x86-64 可重定位代码生成

`-c` / `-r` 使用三个阶段生成 ET_REL：

1. `x86_64.rs` 发射函数机器码，收集 `.rodata`、`.data`、`.bss` 和
   `section` 属性产生的动态节，同时记录尚未绑定的符号引用。
2. 按 ELF 规则构造 `STN_UNDEF → LOCAL → GLOBAL/WEAK` 符号表，并把
   declaration 的 weak/visibility/alignment 信息传递到符号和节布局。
3. 符号索引稳定后生成 `.rela.text`、`.rela.data` 及动态
   `.rela.<section>`，最后由 `elf.rs` 动态构造 `.shstrtab` 和节头表。

自定义代码节中的重定位会从 `.rela.text` 迁移到对应 relocation section；
未初始化的普通全局对象进入 `.bss`，不占目标文件数据空间。

## 链接与 LTO（`-r` / `-flto`）

多 `.o` 可重定位链接由 `driver.rs` 派发到 `link.rs`：

1. 读取各 TU 的 `.llvm.lto` 摘要（`lto.rs`），合并函数定义/引用。
2. 扫描输入 ELF 的 `.rela.*` 表标记跨 TU 引用（`elf.rs`）。
3. 调用 `ld -r --gc-sections` 合并目标文件；`-flto` 时默认
   `-ffunction-sections`，每函数位于 `.text.<name>` 节。
4. 用 `objcopy` 注入合并后的 `.llvm.lto`，并对 dead local 符号
   执行 `--strip-symbol` 补充剔除。

远程验收：`scripts/sync_remote_build.sh` → `kernel_full_check.sh`。

## x86-64 后端 ABI 约定

- 普通标量值使用 `rax`，`__int128` 使用 `rax:rdx`（低 64 位在 `rax`）。
- 后端以 `r13` 作为稳定帧基址。嵌套函数调用通过 `r10` 传递静态链，
  被调函数保存父级 `r13` 后按已解析的父帧偏移访问捕获变量。
- 简化 `va_list` 会在可变参数函数序言中按
  `rdi,rsi,rdx,rcx,r8,r9` 保存 GP 参数，并按 `xmm0`–`xmm7` 保存 SSE
  参数；`va_arg` 支持 float/double/long double。
- `register asm("reg")` 变量直接映射到指定物理寄存器；调用者仍需遵守
  目标寄存器的 SysV caller/callee-saved 约束。

## GNU 控制流扩展

Parser 直接表示 `&&label`、`goto *expr` 和 `asm goto`。标签地址使用
RIP-relative LEA，计算 goto 发射间接 `jmp`，`asm goto` 在 Mini x86
汇编器之前将 `%lN` / `%l[name]` 解析到函数内标签。

Mini x86 汇编器（`codegen/x86_64.rs`）要点：

- **标签命名空间**：C 层 `Stmt::Label` 与 asm 字符串内命名标签（如 `found:`）
  共用 `Codegen::label()`；`-O2` DCE 在 `return` 后仍保留可达标签。
- **同行标签**：`N:insn`（GNU 局部数字标签）与 `found: ret` 在发射前拆成
  标签行 + 指令行（`expand_label_insn_lines`）。
- **局部数字标签**：`6:` / `6f` / `6b` 映射为内部符号（`AsmLocalLabels`）。
- **指令子集**：除 mov/alu/jcc 外，含 `btl/bts/btr`、`shld/shrd`、`rdmsr/wrmsr`
  等；未识别指令 warning + nop 占位。

预处理 `#` 字符串化遵循 C 规则：实参内换行与连续空白折叠为单空格
（`NOFAIL(ptr)` → `"malloc(...)"` 单行字面量）。

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
