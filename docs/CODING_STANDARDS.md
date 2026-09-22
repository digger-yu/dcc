# DCC 代码生成模块架构规范

本文档定义了 `src/codegen/` 目录的组织方式和命名规范。

## 🏗️ 双层结构标准

每个后端架构应采用以下组织方式：

```bash
src/codegen/
├── {arch}.rs                    # 顶层入口 (~300 lines max)
│                                 # - Codegen struct definition
│                                 # - Core interface methods
│                                 # - Re-export submodules
│
└── {arch}/                       # 子模块目录
    ├── mod.rs                   # 子模块声明 (Rust module system)
    ├── backend.rs               # Backend trait implementation (REQUIRED!)
    ├── register.rs              # Register management
    ├── calling_conv.rs          # ABI/calling convention
    ├── dwarf.rs                 # DWARF debug info generation
    ├── sanitizer.rs             # ASAN/HWASAN support
    ├── abi/                     # Architecture-specific ABI helpers
    ├── asm/                     # Assembly encoding templates
    ├── emit/                    # IR lowering to assembly
    ├── mem/                     # Memory addressing modes
    ├── special/                 # Special features (__int128, TLS, etc.)
    └── parallel/                # Parallel compilation orchestration
```

## 📝 设计原则

### 1. **Separation of Concerns (关注点分离)**
- `{arch}.rs`: 核心数据结构和主要入口点
- `{arch}/` 子目录：按功能划分的细粒度模块

### 2. **Consistency (一致性)**
- 所有架构统一使用此模式
- 每个架构必须包含 `backend.rs`
- 顶层 `.rs` 文件大小控制在 300 行以内

### 3. **Scalability (可扩展性)**
- 新功能优先添加到对应子目录
- 避免 `{arch}.rs` 超过 300 行时，创建新的子模块
- 子模块之间通过清晰的接口通信

### 4. **Testability (可测试性)**
- 每个子模块可独立测试
- Mock 机制清晰定义
- Integration tests 覆盖端到端流程

## 🎯 示例：AArch64 (参考实现)

查看 `src/codegen/aarch64.rs` 作为最佳实践范例。

## 🔄 迁移路径

现有架构的迁移计划：

| 架构 | 当前状态 | 目标状态 | 优先级 |
|------|----------|----------|--------|
| aarch64 | ✅ 已规范 | ✅ 维持 | N/A |
| x86_64 | ✅ 刚规范 | ✅ 维持 | N/A |
| mips | ⚠️ 部分规范 | ✅ 完全规范 | Low |
| riscv64 | ⚠️ 单层结构 | ✅ 双层 | Medium |
| arm | ⚠️ 单层结构 | ✅ 双层 | Medium |
| mips64 | ⚠️ 太简单 | ✅ 保持或简化 | Low |

## 📋 检查清单

新添加架构时必须：
- [ ] 创建 `{arch}.rs` 顶层入口文件
- [ ] 创建 `{arch}/backend.rs` Backend trait 实现
- [ ] 在 `{arch}/mod.rs` 中正确声明子模块
- [ ] 所有导入都使用 `crate::codegen::{arch}::...`
- [ ] 编写单元测试覆盖核心功能
- [ ] 更新 `docs/CODING_STANDARDS.md` 如有需要

## 🔧 常见错误避免

❌ 错误做法：
- 将大量代码全部放在单个文件中 (> 3K LOC)
- 忽略 backend.rs 导致无法集成到统一框架
- 跨架构重复代码不提取公共模块

✅ 正确做法：
- 单文件限制在 300-500 行
- 所有架构都有 backend.rs
- 提取通用逻辑到 `src/codegen/common/`

## 💡 提示

- 遇到文件过大？→ 拆分到子模块
- 找不到某个功能应该放哪？→ 看 aarch64 是怎么组织的
- 不确定是否遵循规范？→ 对照本 checklist
