# DCC 设计决策记录 (ADR)

> 第一性原理: 每个决策写下来,新成员能快速理解"为什么是这个"。

## ADR-001: 用 Rust 写,不写 C++

- **问题**: 编译器用什么语言写?
- **候选**: C / C++ / Rust / OCaml / Haskell
- **第一性原理**:
  - **C** = 性能上限最高, 但 unsafe 太多, 自举需要 C 编译器
  - **C++** = GCC 这么干, 但代码膨胀严重 (语法糖过多)
  - **OCaml/Haskell** = 学术编译器常用, 性能不行
  - **Rust** = 性能 = C, 内存安全, 模式匹配写 parser 极简
- **决策**: Rust
- **代价**: 自举需要先有 rustc (但 rustc 几乎所有环境都有)

## ADR-002: AST 当 IR, 不引入 LLVM/Cranelift

- **问题**: 用 LLVM 还是自己写?
- **第一性原理**:
  - LLVM = 110 万行 C++, 编一遍 5 分钟
  - Cranelift = 比 LLVM 简单, 但仍是依赖 (inkwell 等)
  - 自己写 = 1000 行搞定, 性能足够
- **决策**: 自己写
- **代价**: 优化 pass 数量有限; 但 Linux 内核只用 -O0~O2, 足够

## ADR-003: 全局类型注册表 vs TU 局部

- **问题**: 类型是 per-TU 还是全局共享?
- **第一性原理**:
  - GCC 走 per-TU, 简单但每个 TU 重定义 stdarg.h 的类型
  - 全局共享 = Linux 3 万头文件只需解析一次
- **决策**: 全局
- **代价**: 增量更新要谨慎, 但 -50% 编译时间值得

## ADR-004: 默认 gnu23, 不默认 c11

- **问题**: 默认哪个 C 标准?
- **第一性原理**:
  - 现代 Linux 内核用 gnu11
  - 但 -std=gnu23 是 GCC 15 默认
  - 跟 GCC 默认一致, 用户迁移成本最低
- **决策**: gnu23
- **代价**: 严格 C89 代码可能需要显式 -std=c89

## ADR-005: suckless 风格 - 零 crate 依赖

- **问题**: 是否用 anyhow/serde/clap?
- **第一性原理**:
  - CLI 解析 = 200 行手写
  - bincode 序列化类型表 = 我们手写 50 行
  - 每个 crate 都增加 5-30s 编译时间
- **决策**: 零依赖
- **代价**: 一些便利库用不到, 但代码可读性反而提高

## ADR-006: 不用 mmap 也行 (M0) - 但 M3 后必须用

- **问题**: 文件 IO 用 read() 还是 mmap()?
- **第一性原理**:
  - M0 阶段 read+copy 足够, 文件小
  - M3+ 处理 Linux 内核 (单文件 5K 行), mmap 减少 1.2-1.5x 时间
- **决策**: M0 走 read, M3 重构为 mmap

## ADR-007: 一开始就做跨平台, 暂以 host 为默认 target

- **问题**: 跨编译什么时候做?
- **第一性原理**:
  - 后端是数据驱动 (target.rs 抽离)
  - 跨编译本质是"换一份 target 数据", 不需要新写代码
- **决策**: 现在就支持 --target=xxx, 实际只实装 x86_64; 其他后端 M7 补

## ADR-008: LTO 摘要 + `--gc-sections` 剔除 dead symbol

- **问题**: 跨 TU LTO 如何在不引入 LLVM bitcode 的情况下去掉 dead local？
- **第一性原理**:
  - 每函数独立节（`-ffunction-sections`）+ 链接期 `--gc-sections`
    是 binutils 原生路径，无需自写 ELF 编辑器删代码。
  - 轻量 `.llvm.lto` 文本摘要记录 defined/referenced/size，供
    verbose 诊断与 objcopy strip 补充。
- **决策**: `-flto` 默认开启 per-function section；`link.rs` 用
  `ld -r --gc-sections`，并从 `.rela.*` 标记引用。
- **代价**: 依赖 host `ld`/`objcopy`；Windows 本地无法完整验证链接路径。
