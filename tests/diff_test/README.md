# DCC 差分测试工具

一个用于比较 dcc 和 gcc 编译输出的自动化测试工具，专注于发现编码器和链接器中的细微差异。

## 设计理念

这个工具基于一个关键洞察：**手写编译器 + 手写链接器** 的组合意味着：
- 可以精确控制「我期望生成什么编码」
- 可以精确控制「我期望产出什么重定位」
- 差分测试能定位到「是编码错了还是重定位填错了」

一旦跑起来，会成批成批地暴露编码器 + 链接器里「差一个 REX 位 / 差一个 relocation 类型」的 bug。

## 功能特性

### MVP 阶段（当前）
- ✅ **符号表对比**：比较符号的类型、绑定、大小
- ✅ **重定位表对比**：比较重定位类型（如 `R_X86_64_PC32` vs `R_X86_64_PLT32`）
- ✅ **结构化报告**：不是文本 diff，而是语义对比
- ✅ **零误报**：只报告真正的差异

### 计划中的完整版
- ⏳ **指令级语义对比**：捕捉 REX prefix 编码 bug
- ⏳ **等价编码归一化**：处理等价指令避免噪音

## 快速开始

```bash
# 确保已构建 dcc
make build

# 运行所有测试
./tests/diff_test/diff_test.sh

# 运行单个测试
./tests/diff_test/diff_test.sh simple_add

# 列出可用测试
./tests/diff_test/diff_test.sh --list

# 详细输出
./tests/diff_test/diff_test.sh --verbose
```

## 输出示例

```
╔═══════════════════════════════════════════╗
║     DCC DIFFERENTIAL TEST SUITE           ║
╚═══════════════════════════════════════════╝

[INFO] Testing: simple_add
Extracted: /tmp/dcc-diff-test-xxx/simple_add/simple_add_dcc.{symbols,relocs,sections}
Extracted: /tmp/dcc-diff-test-xxx/simple_add/simple_add_gcc.{symbols,relocs,sections}
Report generated: /root/dcc/tests/diff_test/reports/simple_add_report.txt

┌─────────────────────────────────────────┐
│           TEST SUMMARY                  │
├─────────────────────────────────────────┤
│  ✗ Found 2 relocation type mismatches   │
│                                         │
│  Full report: reports/simple_add_report.txt
└─────────────────────────────────────────┘
```

## 报告格式

报告包含两部分：

### 符号表对比
```
========================================
         SYMBOL TABLE COMPARISON
========================================

[i] Symbols only in GCC:
  - simple_add.c

[✓] Common symbols attribute comparison:
  ✗ call_external: TYPE(dcc=FUNC vs gcc=NOTYPE)

Summary:
  DCC-only symbols: 0
  GCC-only symbols: 1
  Attribute mismatches: 1
```

### 重定位表对比
```
========================================
       RELOCATION TABLE COMPARISON
========================================

Relocations by symbol:

  ✗ global_var: type mismatch
    DCC: R_X86_64_PC32
    GCC: R_X86_64_PLT32

Relocation type distribution:
  DCC:
    1 x R_X86_64_PLT32
    1 x R_X86_64_PC32
  GCC:
    6 x R_X86_64_PC32
    1 x R_X86_64_PLT32
```

## 添加新测试

在 `fixtures/` 目录下创建新的 `.c` 文件即可：

```bash
# 创建新测试用例
cat > tests/diff_test/fixtures/my_test.c << 'EOF'
int my_function(int x) {
    return x * 2;
}
EOF

# 运行测试
./tests/diff_test/diff_test.sh my_test
```

## 常见差异类型

### 1. 重定位类型差异
```
R_X86_64_PC32 vs R_X86_64_PLT32
```
**原因**：函数调用时，gcc 使用 PLT32（PIC 兼容），dcc 可能使用 PC32。

**修复**：在 codegen 中为函数调用选择正确的重定位类型。

### 2. 符号类型差异
```
FUNC vs NOTYPE
```
**原因**：未定义的外部引用，gcc 标记为 NOTYPE，dcc 可能标记为 FUNC。

**修复**：在符号表生成时，对外部引用使用 NOTYPE。

### 3. 代码大小差异
```
SIZE(dcc=78 vs gcc=24)
```
**原因**：dcc 未优化，gcc -O0 仍有基本的代码生成优化。

**处理**：这通常是预期的，不需要修复。

## 架构

```
tests/diff_test/
├── diff_test.sh          # 主入口脚本
├── lib/
│   ├── elf_dump.sh       # ELF 信息提取（符号表、重定位）
│   └── compare.sh        # 结构化对比逻辑
├── fixtures/             # 测试用例
│   ├── simple_add.c
│   └── struct_test.c
└── reports/              # 生成的报告
```

## 依赖

- `dcc` (target/release/dcc)
- `gcc`
- `readelf`
- `objdump` (未来扩展)

## 为什么这个方法特别有效

1. **dcc 的 codegen 是手写的** → 可以精确控制「我期望生成什么编码」
2. **dcc 的 linker 是手写的** → 可以精确控制「我期望产出什么重定位」
3. **两层都在你手里** → 差分测试能定位到「是编码错了还是重定位填错了」

而外部工具链用户做不到这一点。

## 下一步

- [ ] 添加指令级对比（需要反汇编和归一化）
- [ ] 支持更多架构（aarch64, riscv64）
- [ ] 集成到 CI 流程
- [ ] 添加更多测试用例覆盖边界情况
