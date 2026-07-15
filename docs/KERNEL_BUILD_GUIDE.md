# DCC 手工编译 Linux 内核指南

> 目标版本：**Linux 6.6 / x86_64**。远程验收目录：`~/dcc-agent-linux66`（独立同步，不覆盖旧仓库）。

## 当前进度（2026-07-15 远程 debug）

| 阶段 | 状态 |
|------|------|
| DCC release 构建 + `cargo test`（22 integration） | ✅ 远程通过 |
| `scripts/test_kernel_features.sh` | ✅ 7/7 |
| `scripts/remote_feature_check.sh` | ✅ 含 `-S` custom section / lock cmpxchg |
| `scripts/sync_remote_build.sh` | ✅ rsync + 全量远程测试 + autoconf 自愈 |
| Kbuild `prepare0` → mod/empty.c | ✅ |
| Kbuild `devicetable-offsets.c`（直接 dcc -S） | ✅ 50+ SIZE_* 行（需 `autoconf.h`） |
| Kbuild `kernel/bounds.c`（直接 dcc -S） | ✅ GNU case range + 逗号数组 |
| `-S` lock/SSE 反汇编 + 自定义节分组 | ✅ `src/codegen/disasm.rs` + integration |
| `-g` DWARF/BTF、`-flto`、sanitizer、`.S` 驱动 | ✅ 初版 |
| `-MD` 依赖、`-O1` 常量折叠、GlobalAsm、`-mno-red-zone` | ✅ integration 覆盖 |
| Host GCC include（modpost `stdbool.h`） | ✅ `gcc -print-file-name=include` 探测 |
| modpost `file2alias.c` 单 TU | ❌ parser 缺口，待修 |
| 全树 `make` / vmlinux / bzImage / QEMU | ⚠️ 待完整链接与 QEMU 环境 |

### 远程同步构建

```bash
# 从开发机（含 Windows Git Bash / WSL）执行
bash scripts/sync_remote_build.sh
```

默认同步到 `ds@192.168.148.128:~/dcc-agent-linux66` 并依次运行：

1. `cargo build --release` + `cargo test --release`
2. `scripts/remote_feature_check.sh`（含 `-S` 自定义节 / lock cmpxchg）
3. `scripts/test_kernel_features.sh`
4. 内核单 TU：`devicetable-offsets.s`、`bounds.s`

**Windows 注意**：`*.sh` 必须为 LF 行尾。若远程报 `pipefail: invalid option`，
在本地执行 `git ls-files --eol scripts/*.sh` 确认工作区为 `w/lf`，再重新同步。

**内核头文件**：`devicetable-offsets.c` 依赖 `include/generated/autoconf.h`。
若内核树仅有 `.config` 而无该头文件，同步脚本会自动执行
`make olddefconfig`（如需）与 `make include/generated/autoconf.h`（使用系统 gcc）。

## 1. 环境要求

### 1.1 硬件/系统

| 项目 | 最低要求 |
|------|---------|
| 架构 | x86_64 |
| 操作系统 | Linux (推荐 Ubuntu 22.04+ / Debian 12+) |
| 内存 | ≥ 2 GB |
| 磁盘 | ≥ 10 GB 可用空间 |

### 1.2 软件依赖

```bash
# 基础构建工具
sudo apt update
sudo apt install -y build-essential gcc make bc flex bison libssl-dev libelf-dev

# Rust 工具链 (用于编译 DCC)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Linux 内核源码 (6.1 版本)
cd /usr/src
sudo wget https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.1.tar.xz
sudo tar xf linux-6.1.tar.xz
sudo ln -s linux-6.1 linux   # 方便引用
```

### 1.3 验证环境

```bash
gcc --version        # 确认 GCC 可用 (用于对比)
make --version       # 确认 make 可用
rustc --version      # 确认 Rust 可用
```

## 2. 编译 DCC

### 2.1 获取源码

```bash
cd ~
git clone <dcc-repo-url> dcc
cd dcc
```

### 2.2 构建

```bash
cargo build --release
```

构建产物位于 `target/release/dcc`。

### 2.3 安装到 PATH (可选)

```bash
sudo cp target/release/dcc /usr/local/bin/dcc
dcc --version   # 验证
```

## 3. 内核配置

### 3.1 使用默认配置

```bash
cd /usr/src/linux
make defconfig
```

### 3.2 调整配置 (推荐)

为了减少编译复杂度，建议关闭一些 DCC 尚未完全支持的特性：

```bash
# 使用 menuconfig 或直接编辑 .config
make menuconfig
```

建议关闭的选项：
- `CONFIG_LTO` (Link Time Optimization) — 需要特殊编译器支持
- `CONFIG_RETPOLINE` — 涉及复杂汇编
- `CONFIG_KASAN` — 内存调试，涉及内部编译器钩子

### 3.3 最小配置 (用于测试)

如果只想测试 DCC 能否编译单个内核文件，可以跳过完整配置，直接编译单个 `.c` 文件。

## 4. 单文件编译测试

### 4.1 预处理测试

验证 DCC 预处理器能否正确处理内核文件：

```bash
cd /usr/src/linux

# 预处理 init/main.c
dcc -E \
    -include include/linux/compiler-version.h \
    -include include/linux/kconfig.h \
    -nostdinc \
    -I include \
    -I arch/x86/include \
    init/main.c > /tmp/main_preprocessed.c 2>/dev/null

# 查看输出行数 (与 GCC 对比)
wc -l /tmp/main_preprocessed.c
gcc -E -include include/linux/compiler-version.h \
    -include include/linux/kconfig.h \
    -nostdinc -I include -I arch/x86/include \
    init/main.c 2>/dev/null | wc -l
```

DCC 的预处理输出应为 GCC 的 1.5-2.0 倍大小。

### 4.2 语法检查

使用 `-fsyntax-only` 模式测试解析器（不生成代码）：

```bash
# 预处理 + 解析
dcc -E \
    -include include/linux/compiler-version.h \
    -include include/linux/kconfig.h \
    -nostdinc -I include -I arch/x86/include \
    init/main.c 2>/dev/null | \
    dcc -fsyntax-only -

# 如果无输出则表示解析成功
echo "Exit code: $?"
```

### 4.3 完整编译 (生成 ELF)

```bash
# 预处理 + 解析 + 代码生成
dcc -E \
    -include include/linux/compiler-version.h \
    -include include/linux/kconfig.h \
    -nostdinc -I include -I arch/x86/include \
    init/main.c 2>/dev/null | \
    dcc -c -o /tmp/main.o -

# 检查输出文件
file /tmp/main.o
```

### 4.4 批量测试多个内核文件

```bash
#!/bin/bash
# test_kernel_files.sh - 测试 DCC 对多个内核文件的编译

DCC=/path/to/dcc
KERN=/usr/src/linux
cd $KERN

INCLUDE_FLAGS="-nostdinc -I include -I arch/x86/include"
FORCE_INCLUDES="-include include/linux/compiler-version.h -include include/linux/kconfig.h"

FILES="init/main.c kernel/panic.c kernel/sys_ni.c kernel/sched/core.c"

for f in $FILES; do
    echo -n "Testing $f ... "
    $DCC -E $FORCE_INCLUDES $INCLUDE_FLAGS "$f" 2>/dev/null | \
        $DCC -fsyntax-only - 2>/tmp/dcc_err
    
    if [ $? -eq 0 ]; then
        echo "PASS"
    else
        ERR_COUNT=$(grep -c 'error:' /tmp/dcc_err)
        echo "FAIL ($ERR_COUNT errors)"
        head -5 /tmp/dcc_err
    fi
done
```

## 5. 使用 make 集成编译

### 5.1 基本用法

```bash
cd /usr/src/linux

# 使用 DCC 作为 C 编译器
make CC=dcc -j$(nproc)

# 或者使用绝对路径
make CC=/usr/local/bin/dcc -j$(nproc)
```

### 5.2 带详细日志

```bash
make CC=dcc V=1 -j1 2>&1 | tee /tmp/dcc_build.log
```

### 5.3 只编译特定目标

```bash
# 只编译 init/main.o
make CC=dcc init/main.o

# 只编译内核核心
make CC=dcc kernel/
```

### 5.4 已知限制

DCC 目前处于开发阶段，完整内核编译可能遇到以下问题：

| 问题 | 原因 | 状态 |
|------|------|------|
| 解析器错误 | 某些复杂宏展开后的构造 | 持续改进中 |
| codegen 不完整 | 部分 x86_64 指令尚未实现 | 持续改进中 |
| 链接失败 | 缺少完整的链接器支持 | 计划中 |

## 6. 调试技巧

### 6.1 查看预处理输出

```bash
dcc -E input.c > /tmp/preprocessed.c
head -100 /tmp/preprocessed.c
```

### 6.2 查看 AST

```bash
dcc -dump-ast input.c
```

### 6.3 详细模式

```bash
dcc -v input.c -o output.o
```

### 6.4 使用 GDB 调试 DCC

```bash
# 在远程 Linux 上编译 debug 版本
cargo build  # debug 模式

# 启动 GDB
gdb ./target/debug/dcc

# 设置断点
(gdb) break parser.rs:100
(gdb) run input.c -fsyntax-only

# 单步调试
(gdb) next
(gdb) print self.cur
```

### 6.5 对比 GCC 预处理输出

```bash
# DCC 预处理
dcc -E input.c > /tmp/dcc_out.c 2>/dev/null

# GCC 预处理
gcc -E input.c > /tmp/gcc_out.c 2>/dev/null

# 对比
diff /tmp/dcc_out.c /tmp/gcc_out.c | head -50
```

## 7. 常见问题

### Q: 报错 "unknown C compiler"

A: 内核构建系统会检测 CC 编译器。确保 DCC 能正确响应版本查询：
```bash
dcc --version
# 应输出: dcc 0.1.0
```

### Q: 预处理输出过大

A: 检查是否有头文件被重复包含。DCC 的 include guard 机制通过文件路径 canonicalize 实现，确保所有路径可正确解析。

### Q: `#error` 误触发

A: 通常是因为宏定义体中包含注释（如 `HZ CONFIG_HZ /* comment */`），导致 `#if` 表达式求值失败。DCC 已内置 `strip_comments()` 处理此情况。如果仍遇到，请报告。

### Q: 解析器报 "expected type" 错误

A: 可能是遇到了 DCC 尚未支持的内核构造。使用 `-dump-ast` 查看已解析的部分，定位错误位置。

## 8. 性能对比

在典型开发机器上的参考数据：

| 文件 | GCC 预处理行数 | DCC 预处理行数 | DCC 解析时间 |
|------|-------------|-------------|------------|
| init/main.c | ~95K | ~161K (1.7x) | < 0.5s |
| kernel/panic.c | ~57K | ~108K (1.9x) | < 0.3s |
| kernel/sys_ni.c | ~5K | ~18K (3.6x) | < 0.1s |

## 9. 参考资料

- DCC 源码: `src/preproc.rs` (预处理器), `src/parser.rs` (解析器)
- Linux 内核构建系统: `Documentation/kbuild/` 
- GCC 兼容性: `docs/ROADMAP.md`
