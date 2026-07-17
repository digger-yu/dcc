# DCC 手工编译 Linux 内核指南

> 目标版本：**Linux 6.6 / x86_64**。远程验收目录：`~/dcc-agent-linux66`（独立同步，不覆盖旧仓库）。

## 当前进度（2026-07-17）

| 阶段 | 状态 |
|------|------|
| DCC release 构建 + `cargo test`（integration + errors snapshot） | ✅ 远程通过 |
| `scripts/test_kernel_features.sh` | ✅ 7/7 |
| `scripts/remote_feature_check.sh` | ✅ 17/17（含 LTO per-function section） |
| `scripts/prepare0_check.sh` | ✅ devicetable / bounds / file2alias 窄目标 |
| `scripts/pahole_regress.sh` / `objtool_regress.sh` | ✅ BTF struct + eh_frame FDE |
| `scripts/sync_remote_build.sh` | ✅ rsync + 全量远程测试 + autoconf 自愈 |
| Kbuild `devicetable-offsets.c`（直接 dcc -S） | ✅ 50+ SIZE_* 行 |
| Kbuild `kernel/bounds.c`（直接 dcc -S） | ✅ |
| `arch/x86/kernel/asm-offsets.c` 预处理 + 解析 + 代码生成 | ✅ 2026-07-17：867→0 解析；0 语义；`.s` 已生成 |
| modpost `file2alias.c` 单 TU | ✅ |
| LTO 链接 `-r` 多 `.o` + dead symbol gc | ✅ `ld -r --gc-sections` + `.llvm.lto` |
| `scripts/modpost_link.sh` | ✅ Rust fixdep + dcc 三 TU（modpost/file2alias/sumversion） |
| 全树 `make CC=dcc` / vmlinux / bzImage / QEMU | ⚠️ 脚本就绪 | 步骤 6–7/10 需 `BUILD_*=1`；无产物 SKIP |

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
5. `prepare0_check.sh`、`pahole_regress.sh`、`objtool_regress.sh`
6. `kernel_phase_b_check.sh` 或 `kernel_full_check.sh`（modpost / prepare0 / QEMU / vmlinux）

### 阶段 B 质量门禁（步骤 7–10）

```bash
# 远程（脚本须 LF；Windows scp 后执行 perl -pi -e 's/\x0d//g' scripts/*.sh）
bash scripts/kernel_phase_b_check.sh          # modpost + prepare0 → PASS
bash scripts/vmlinux_regress.sh               # 无 vmlinux 时 SKIP
bash scripts/qemu_boot_matrix.sh              # 无 bzImage 时 SKIP
BUILD_BZIMAGE=1 BUILD_VMLINUX=1 bash scripts/kernel_build_smoke.sh  # 全树 smoke
```

| 脚本 | 用途 |
|------|------|
| `build_fixdep_dcc.sh` | 安装 Rust `fixdep`（不依赖 gcc 编译 C fixdep） |
| `dcc-host-wrapper.sh` | HOSTCC 包装：dcc -c + gcc 链接；fixdep 走 Rust |
| `gen_elfconfig_h.sh` | 从 empty.o 生成 elfconfig.h |
| `prepare0_full.sh` | prepare0 全链路（等价窄目标 + modpost） |
| `kernel_phase_b_check.sh` | 阶段 B 编排 |
| `pahole_regress.sh` / `objtool_regress.sh` | 单 TU BTF / eh_frame 回归 |
| `qemu_boot_matrix.sh` | QEMU 加载 bzImage（无镜像 SKIP） |
| `vmlinux_regress.sh` | pahole -J / objtool check vmlinux |
| `kernel_full_check.sh` | 编排 modpost + QEMU + vmlinux（非阻塞） |
| `remote_libiberty_probe.sh` | GCC libiberty 纯 C 子集探测 |

### LTO 与 dead symbol 剔除

```bash
# 编译：-flto 默认开启 -ffunction-sections（.text.<func> 节）
dcc -flto -c -o a.o a.c
dcc -flto -c -o b.o b.c

# 链接：合并 .llvm.lto 摘要，ld --gc-sections 剔除无引用节
dcc -r -flto -o merged.o a.o b.o
```

**Windows 注意**：`*.sh` 必须为 LF 行尾。若远程报 `pipefail: invalid option`，
在本地执行 `git ls-files --eol scripts/*.sh` 确认工作区为 `w/lf`，再重新同步。
scp 后可在远程执行 `perl -pi -e 's/\x0d//g' scripts/*.sh` 去除 CR。

**prepare0 豁免**：`make prepare0 CC=dcc` 会触发 Kbuild 重建宿主工具（kconfig/conf 等），
dcc 宿主 C 尚未覆盖。请用 `scripts/prepare0_full.sh` 验收等价窄目标 + Rust fixdep + modpost。

**全树 smoke**：`make CC=dcc HOSTCC=gcc bzImage` 为当前推荐混合模式（内核 TU 用 dcc，
kconfig/fixdep 等宿主工具用 gcc 或 Rust fixdep）。步骤 7/10 需先产出 vmlinux/bzImage。

### asm-offsets 验收（prepare0 步骤 4–5）

Linux 6.6 x86_64 在 `prepare0` 中会编译 `arch/x86/kernel/asm-offsets.c` 生成
`asm-offsets.s`（供后续提取结构体偏移常量）。

```bash
# 远程（内核树 ~/linux-6.6，dcc ~/dcc-agent-linux66/target/release/dcc）
python3 scripts/archive/test_asm_offsets.py

# 或手动：读取 .asm-offsets.s.cmd 中的 gcc 命令，将 gcc 替换为 dcc 后执行
cd ~/linux-6.6
dcc -Wp,-MMD,... -S -o arch/x86/kernel/asm-offsets.s arch/x86/kernel/asm-offsets.c
```

**2026-07-17 进展**：

| 指标 | 修复前 | 修复后 |
|------|--------|--------|
| 解析错误（E0xxx） | ~867 | **0** |
| 语义错误（E1xxx） | 6 | **0** |
| `asm-offsets.s` | 未生成 | **已生成**（~5 MB） |

**2026-07-16** 主要修复：语句表达式后缀调用、typeof(struct)、section 相邻字符串、
asm goto 试探解析、位域逗号分隔、结构体空字段、__auto_type、typedef 函数括号、
`__builtin_offsetof` 常量求值、object-like→function-like 宏链、多维数组字段、
for-init 枚举声明、属性后指针函数声明、GNU `?:` 省略中间操作数等。

**2026-07-17** 补充：include 缓存零参 function-like 宏（V6）、三元指针类型、
内联 asm GNU 局部数字标签（`6:`/`6f`/`6b`）、**`-O2` 保留 `goto` 目标 C 标签**、
**同行标签拆分**（`1:jmp` / `found: ret`）、**mini-asm 位测试/shld/shrd/bsf/popcnt/bswap/movnt**、
**GAS 伪指令**（`.pushsection`/`.balign`/`.long`/`.quad`）、**lock/rep 前缀**。

**modpost 宿主 C（2026-07-17）**：

| 修复项 | 说明 |
|--------|------|
| `NOFAIL` / `#` 字符串化 | 多行实参折叠为单行字面量，消除 `modpost.c:5560` Invalid token |
| `struct __attribute__((__may_alias__))` | 解析器在 struct/enum 名之前跳过属性 |
| 兼容 typedef 重定义 | glibc/linux 头重复 `dev_t`/`uint64_t` 等 → `register_typedef` |
| 局部遮蔽 libc 名 | `index`/`basename` 等不再 E0007 |
| `typeof(_Generic(...))` | READ_ONCE 类宏；声明初始化 `sizeof(*v)` |
| asm `#` 行内注释 | `jump_label.h` `jmp %l[l_yes] # objtool NOPs this` |

辅助脚本：`scripts/archive/test_asm_offsets.py`（一键替换 gcc 为 dcc 并统计错误）、
`scripts/archive/summarize_err.py`（解析/语义错误分类）。

**Windows 同步注意**：scp 后须 `perl -pi -e 's/\x0d//g'` 去 CR；勿用 `sed 's/\r//g'`。

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
| 解析器错误 | 复杂宏展开后的 GNU 构造 | asm-offsets **解析已通过**（2026-07-16） |
| 语义/类型检查 | percpu、thread_info 等内联宏展开类型不完整 | asm-offsets **已通过**（2026-07-17） |
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

A: 可能是遇到了 DCC 尚未支持的内核构造。使用 `-dump-ast` 查看已解析的部分，
定位错误位置。`asm-offsets.c` 已于 2026-07-17 全链路通过；历史问题可用
`scripts/archive/summarize_err.py` 区分解析与语义错误。

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
