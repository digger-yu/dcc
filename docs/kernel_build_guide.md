# DCC 手工编译 Linux 内核指南

> 目标版本：**Linux 7.1 / x86_64**。本地验收目录：`./`。

## 当前进度（2026-10-26）

本轮只推进「让内核 C 语料能被 dcc 编出」这条链上的**内联与常量求值**缺口；全树跑到第 48 个 TU
被主动中止，**`vmlinux.o` 及其后仍未验证**。逐项数字与失败清单见
`report/linux71-dcc-fulltree-build-2026-10-26.md`（`report/` 不入库，是一份本地一次性产物）。

| 项 | 状态 |
|------|------|
| 「只能接立即数」的约束拿到运行时值 | ✅ 改为当场硬失败，并在诊断里点名表达式（`expression: …`），不再静默发出指向寄存器名的野重定位 |
| `asm goto` 形体的 `__always_inline`（`jump_label.h` 的 `arch_static_branch`） | ✅ 可内联；标签定义、`goto` 与模板里的 `%l[…]`/`%c[…]` 文本一起改名 |
| `static_cpu_has`/`cpu_feature_enabled` 宏链（`cpufeature.h`） | ✅ 三元先折子表达式再判条件；`"i"` 操作数从形参临时量代回实参；`testb $imm, sym(%rip)` 与 GAS 逐字节一致 |
| `__WARN_bug_entry` 的 `__flags` 局部常量（`asm/bug.h`） | ✅ 写一次的常量局部与常量重赋值都能代回，按声明块压/弹作用域 |
| `call_on_stack`（`asm/irq_stack.h`） | ⚠️ 函数设计符 `"i"` 与 `call 符号` 已修；`GP 寄存器池耗尽` 未修 |
| `rip_rel_ptr`（`asm/asm.h:121`） | ❌ 仍有 `expression: p` 站点没展开（缺调用点的值专化） |
| 全树 `make CC=dcc` | ⏸ 第 48 个 TU 处 3 个失败，本轮主动中止 |
| `vmlinux.o` / `vmlinux` / `bzImage` / QEMU | ⏸ 未到达（上一轮挡在 objtool，本轮挡在更早的编译步骤） |
| `make test` | ✅ 3887 passed / 0 failed / 7 ignored；`gcc.c-torture` 3707 pass / 0 fail / 6 skip（pre-commit 闸门实跑两次） |

一条通用结论值得单独记住：**`"i"`/`"I"`/`%c[…]` 这类约束是「链接期常量」的契约，不是优化建议**。
展开/折叠链上任一环把它当成寄存器，产物就带着一条解不开的重定位出门，而编译器自己一声不响；
dcc 现在的立场是：给不出常量就当场硬失败，并把表达式原文写进诊断，让缺口在全树第一轮就能被列齐。

## 历史进度（2026-10-25）

全树重跑与缺陷修复的一轮。结论：**内核 C 语料已 100% 由 dcc 编出（零 gcc 回退），但 `vmlinux.o` 仍未打通**。

| 阶段 | 状态 |
|------|------|
| 全树 `make CC=dcc`：1093 个内核 `.o` | ✅ 全部由 dcc 编出，日志内 error 行 0、`falling back` 0 |
| `built-in.a` 归档 458 个 | ✅ |
| `vmlinux.o`（`ld -r` + objtool） | ❌ objtool 报 120 条 `empty alternative entry` + 1 条 `can't find orig instruction`（ALTERNATIVE 表条目的 `orig_len` 字节为 0）。注意：这份清单是从中间版本二进制编出的 .o 采集的，purge 重跑后需重采 |
| `vmlinux` / `bzImage` / QEMU | ⏸ 被上一项挡住，未到达 |
| `cargo test --release --lib` / `--test integration` | ✅ 2576 / 1187+5 ignored 全通过（本轮新增 12 条用例，逐条做过「撤掉修复即变红」反向验证） |

本轮修掉的 9 类缺陷（症状 → 根因 → 落点）见 `report/linux71-dcc-fulltree-build-2026-10-25.md`；
其中三项影响所有架构的通用正确性：

1. **类型身份与类型相等不能用派生 `Debug`/派生结构相等**：内核类型是「先前向声明、后补全」
   且带环的共享 DAG，派生实现一边把类型串算成 GB 级（编译被 OOM 杀），一边把同一个 tag
   判成两个类型（`EXPORT_SYMBOL_GPL` 的 `extern typeof(sym) sym;` 误报 E0007）。
2. **弱符号与节属性是链接期契约**：`__weak` 在「属性写在 `*` 之后」「只写在原型上」两种写法下
   丢失、函数类型的无体声明被当 tentative definition 发进 `.bss`，都会在 `ld -r vmlinux.o`
   变成 360 条 multiple definition。
3. **自定义节的 flags/对齐必须按 GAS 语义**：`SHF_MERGE` 不得在函数级并行合并时被压平，
   非 merge 节默认对齐是 1 不是 8——否则 `ld` 逐对象垫边界，objtool 的
   「节长必须是条目长整倍数」立刻破（`.altinstructions` 14、`__ex_table` 12）。

## 历史进度（2026-08-27）

> 下表是 2026-08-27 那一次基于当时 HEAD 的记录；其中「全树 vmlinux + QEMU 通过」一行
> 在 2026-10-25 用当前 HEAD 复现不出来（上表是本次实测结论），保留仅作历史基线。

| 阶段 | 状态 |
|------|------|
| DCC release 构建 + `cargo test`（integration + errors snapshot） | ✅ 本地通过 |
| `scripts/test_kernel_features.sh` | ✅ 7/7 |
| `scripts/prepare0_check.sh` | ✅ devicetable / bounds / file2alias 窄目标 |
| `scripts/pahole_regress.sh` / `objtool_regress.sh` | ✅ BTF struct + eh_frame FDE |
| Kbuild `devicetable-offsets.c`（直接 dcc -S） | ✅ 50+ SIZE_* 行 |
| Kbuild `kernel/bounds.c`（直接 dcc -S） | ✅ |
| `arch/x86/kernel/asm-offsets.c` 预处理 + 解析 + 代码生成 | ✅ 2026-07-17：867→0 解析；0 语义；`.s` 已生成 |
| modpost `file2alias.c` 单 TU | ✅ |
| LTO 链接 `-r` 多 `.o` + dead symbol gc | ✅ `ld -r --gc-sections` + `.llvm.lto` |
| `scripts/modpost_link.sh` | ✅ Rust fixdep + dcc 三 TU（modpost/file2alias/sumversion） |
| 全树 `make CC=dcc` / vmlinux / bzImage / QEMU | ✅ **vmlinux + QEMU boot 验证通过**（16MB ELF，串口启动日志正常） |
| **initramfs 启动到 init** | ✅ **已验证**（内核成功执行 "Run /init as init process"） |

### 本地一键验证

```bash
# 在 dcc 仓库根目录执行
bash scripts/test_kernel_features.sh
bash scripts/prepare0_check.sh
bash scripts/pahole_regress.sh
bash scripts/objtool_regress.sh
```

默认在 `./` 仓库内依次运行：

1. `cargo build --release` + `cargo test --release`
2. `scripts/test_kernel_features.sh`（含 `-S` 自定义节 / lock cmpxchg）
3. 内核单 TU：`devicetable-offsets.s`、`bounds.s`
4. `prepare0_check.sh`、`pahole_regress.sh`、`objtool_regress.sh`
5. `kernel_full_check.sh`（modpost / prepare0 / QEMU / vmlinux）

### 阶段 B 质量门禁（步骤 7–10）

```bash
# 本地（脚本须 LF；Windows scp 后执行 perl -pi -e 's/\x0d//g' scripts/*.sh）
bash scripts/vmlinux_regress.sh               # 无 vmlinux 时 SKIP
bash scripts/qemu_boot_matrix.sh              # 无 bzImage 时 SKIP；有 bzImage 时硬门禁
BUILD_BZIMAGE=1 BUILD_VMLINUX=1 bash scripts/kernel_build_smoke.sh  # 全树 smoke
STRICT_FULL_BUILD=1 bash scripts/kernel_full_check.sh  # strict: 禁止核心门禁 SKIP
```

`STRICT_FULL_BUILD=1` 会强制启用 `BUILD_VMLINUX=1` 与 `BUILD_BZIMAGE=1`，
并要求 `modpost` 三个 TU 全部由 dcc 编译、`vmlinux` / `bzImage` 产物存在，
且 `pahole` / `objtool` 必须安装并通过检查；上述核心门禁出现缺失产物、
缺失工具、非零退出或复用旧 modpost 的情况都会返回失败，不再按 SKIP 处理。

| 脚本 | 用途 |
|------|------|
| `build_fixdep_dcc.sh` | 安装 Rust `fixdep`（不依赖 gcc 编译 C fixdep） |
| `dcc-host-wrapper.sh` | HOSTCC 包装：dcc -c + gcc 链接；fixdep 走 Rust |
| `gen_elfconfig_h.sh` | 从 empty.o 生成 elfconfig.h |
| `prepare0_full.sh` | prepare0 全链路（等价窄目标 + modpost） |
| `kernel_purge_stale_objects.sh` | 清掉「早于当前 dcc 二进制」与「gcc 回退」两类 .o，使全树产物同一版编出（见 §5.0） |
| `pahole_regress.sh` / `objtool_regress.sh` | 单 TU BTF / eh_frame 回归 |
| `qemu_boot_matrix.sh` | QEMU 加载 bzImage；无镜像默认 SKIP，镜像存在后要求 QEMU 与 boot log 关键字符串通过 |
| `vmlinux_regress.sh` | pahole -J / objtool check vmlinux |
| `kernel_full_check.sh` | 编排 modpost + QEMU + vmlinux；strict 模式禁止核心门禁 SKIP，默认模式已有产物仍会执行对应门禁 |

### LTO 与 dead symbol 剔除

```bash
# 编译：-flto 默认开启 -ffunction-sections（.text.<func> 节）
dcc -flto -c -o a.o a.c
dcc -flto -c -o b.o b.c

# 链接：合并 .llvm.lto 摘要，ld --gc-sections 剔除无引用节
dcc -r -flto -o merged.o a.o b.o
```

**Windows 注意**：`*.sh` 必须为 LF 行尾。若在 Windows 编辑后 `pipefail: invalid option`，
在本地执行 `git ls-files --eol scripts/*.sh` 确认工作区为 `w/lf`，再重新保存。
Linux 环境下 LF 是默认, 不需要此步骤.

**prepare0 豁免**：`make prepare0 CC=dcc` 会触发 Kbuild 重建宿主工具（kconfig/conf 等），
dcc 宿主 C 尚未覆盖。请用 `scripts/prepare0_full.sh` 验收等价窄目标 + Rust fixdep + modpost。

**全树 smoke**：`make CC=dcc HOSTCC=gcc bzImage` 为当前推荐混合模式（内核 TU 用 dcc，
kconfig/fixdep 等宿主工具用 gcc 或 Rust fixdep）。步骤 7/10 需先产出 vmlinux/bzImage。

**QEMU 启动验证**：`scripts/qemu_boot_matrix.sh` 使用 `qemu-system-x86_64` 加载
bzImage 并检测启动日志。内核必须启用串口控制台支持：

```bash
# 内核配置必须启用以下选项（defconfig 默认未启用）
CONFIG_SERIAL_8250=y
CONFIG_SERIAL_8250_CONSOLE=y
CONFIG_SERIAL_CORE=y
CONFIG_SERIAL_CORE_CONSOLE=y
```

可通过 `scripts/config` 启用：

```bash
cd /path/to/linux-7.1
scripts/config --enable CONFIG_SERIAL_8250
scripts/config --enable CONFIG_SERIAL_8250_CONSOLE
make olddefconfig
```

启动命令：

```bash
qemu-system-x86_64 \
    -kernel arch/x86/boot/bzImage \
    -m 512M -no-reboot -display none -nographic \
    -append "console=ttyS0 panic=-1"
```

预期输出包含 `Linux version 7.1.0` 等启动日志。
无 initramfs 时最终会因无法挂载根文件系统而 panic，这是预期行为，
`qemu_boot_matrix.sh` 会将此情况视为 boot smoke 通过。

**initramfs 启动验证**：`scripts/build_initramfs.sh` 构建最小 initramfs（busybox + /init 脚本），
内核成功启动后会尝试执行 init 进程：

```bash
# 构建 initramfs
bash scripts/build_initramfs.sh

# 启动带 initrd 的内核
qemu-system-x86_64 \
    -kernel arch/x86/boot/bzImage \
    -initrd initramfs/initramfs.cpio.gz \
    -m 512M -no-reboot -display none -serial stdio \
    -append "console=ttyS0 earlyprintk=serial,ttyS0,115200"
```

预期输出包含：
```
Run /init as init process
=== DCC kernel: init reached successfully ===
=== Boot to init complete ===
```

内核配置必须额外启用：
```bash
CONFIG_BLK_DEV_INITRD=y
CONFIG_RD_GZIP=y
CONFIG_DEVTMPFS=y
CONFIG_DEVTMPFS_MOUNT=y
```

**混合编译模式**：`scripts/dcc-kernel-cc.sh` 包装脚本实现 C 文件用 dcc、汇编文件用 gcc：

```bash
make CC=/path/to/dcc-kernel-cc.sh bzImage
```

这解决了复杂 GAS 宏（如 `ALT_INSTR`、`FILL_RETURN_BUFFER`）在 dcc 预处理后的兼容性问题。

### asm-offsets 验收（prepare0 步骤 4–5）

Linux 7.1 x86_64 在 `prepare0` 中会编译 `arch/x86/kernel/asm-offsets.c` 生成
`asm-offsets.s`（供后续提取结构体偏移常量）。

```bash
# 本地 (内核树 ~/linux-7.1, dcc ~/dcc/target/release/dcc)
# 手动：读取 .asm-offsets.s.cmd 中的 gcc 命令，将 gcc 替换为 dcc 后执行
cd ~/linux-7.1
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
| `struct __attribute__((__may_alias__))` | 解析器完整支持 `may_alias` 类型属性：struct/union/enum 名前、体后、typedef 均可；存储于 `ParsedAttrs.may_alias`，禁用 TBAA，内核 `__bitwise` 类型 (`__le32`/`__be32`) 依赖此属性进行安全类型双关 |
| 兼容 typedef 重定义 | glibc/linux 头重复 `dev_t`/`uint64_t` 等 → `register_typedef` |
| 局部遮蔽 libc 名 | `index`/`basename` 等不再 E0007 |
| `typeof(_Generic(...))` | READ_ONCE 类宏；声明初始化 `sizeof(*v)` |
| asm `#` 行内注释 | `jump_label.h` `jmp %l[l_yes] # objtool NOPs this` |

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

# Linux 内核源码 (7.1 版本)
cd /usr/src
sudo wget https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.1.tar.xz
sudo tar xf linux-7.1.tar.xz
sudo ln -s linux-7.1 linux   # 方便引用
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

### 5.0 修完缺陷后如何清缓存再跑（全树重编标准流程）

Kbuild 的 `if_changed` 只比对**记录的命令行串**，而内核 TU 的命令行里写的是包装脚本路径
（`scripts/dcc-kernel-cc.sh`）。dcc 二进制换了一版，Kbuild 并不认账，于是旧二进制编出的
`.o` 会混进 `vmlinux.o` —— 产物既说不清是哪一版编的，也不可复现。包装脚本在 dcc 失败时
还会回退 gcc，回退产物同样不会被 make 察觉。所以每修一个缺陷都要按「二进制级判据」清一次：

```bash
# 1) 建当前源码的二进制
 cargo build --release -j2

# 2) 清掉「早于该二进制」+「gcc 回退」两类 .o（宿主工具 scripts/、tools/ 与有意
#    交给 gcc 的 arch/x86/realmode 一律不碰）
bash scripts/kernel_purge_stale_objects.sh /path/to/linux-7.1 target/release/dcc

# 3) 只清 gcc 回退产物（本轮改动不改变其它产物时用它，省一次全树重编）
bash scripts/kernel_purge_stale_objects.sh --gcc-fallback-only /path/to/linux-7.1 target/release/dcc

# 4) 再跑
 make CC=/path/to/scripts/dcc-kernel-cc.sh HOSTCC=gcc -k -j3
```

磁盘余量也要盯：dcc 的头文件缓存落在 `~/.dcc/cache`（内容寻址、可安全删），编全树时
每轮都能长到几十 GB，按时间清旧条目即可：`find ~/.dcc/cache -type f -mmin +30 -delete`。

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

DCC 已成功编译完整 Linux 7.1 内核并生成 vmlinux，且 QEMU 启动验证通过，但仍有以下注意事项：

| 问题 | 原因 | 状态 |
|------|------|------|
| 解析器错误 | 复杂宏展开后的 GNU 构造 | asm-offsets **解析已通过**（2026-07-16） |
| 语义/类型检查 | percpu、thread_info 等内联宏展开类型不完整 | asm-offsets **已通过**（2026-07-17） |
| codegen 不完整 | 部分 x86_64 指令尚未实现 | 持续改进中 |
| vmlinux 生成 | 完整内核链接 | ✅ **已完成**（2026-08-25，16MB ELF，零 objtool 错误） |
| QEMU 启动 | bzImage 串口启动日志 | ✅ **已验证**（2026-08-27，需启用 CONFIG_SERIAL_8250_CONSOLE） |
| **initramfs 启动到 init** | 内核执行 "Run /init as init process" | ✅ **已验证**（2026-08-27，busybox init 脚本成功执行） |
| DEFINE() .ascii 输出 | -S 模式下发射 `.ascii "->..."` 行 | ✅ **已完成**（2026-08-28，与 GCC 输出一致） |
| .S 文件预处理 | 续接行处理与 GCC 行为一致 | ✅ **已验证**（entry_64.S 预处理后可通过 GAS 汇编） |
| const_eval offsetof | 复杂内核结构体偏移量求值 | ✅ 当前 HEAD 下 `arch/x86/kernel/asm-offsets.c` 由 dcc 编出 `asm-offsets.s` 并重生 `include/generated/asm-offsets.h`（2026-10-25 实测） |
| .S 文件全链路编译 | dcc 预处理 + gcc fallback | ✅ **已完成**（2026-08-28，dcc-kernel-cc.sh 用 dcc 编译 .S，失败自动回退 gcc；51 个 .S 文件全部处理，QEMU 启动验证通过） |
| 字符字面量跨行扫描 | 注释中 `'` 吞掉后续行宏展开 | ✅ **已修复**（2026-08-28，三处扫描循环增加 `\n` 终止条件） |
| inline asm 的 GAS 宏（`.macro`/`.irp`/`.ifc`/`.set`） | 内核 `_ASM_EXTABLE_TYPE_REG` 的 type 字段靠它 | ✅ **已实现**（2026-10-25：嵌套块尾计数、形参/实参与 `\@`、`.set` 符号表、整数表达式补移位与按位运算、先解析操作数再展开宏） |
| 自定义节的 flags / 对齐（`SHF_MERGE`、`sh_addralign`） | objtool 校「节长 = 条目长整倍数」 | ✅ **已修复**（2026-10-25：并行合并不再压平 flags；非 merge 节默认对齐 1） |
| ALTERNATIVE 表条目长与替换节布局 | `.byte 773b-771b` / `.byte 775f-774f` 算错 ⇒ objtool 报 `empty alternative entry`；`.skip` 根本没实现；可执行 pushsection 的目标节按 16 垫 ⇒ `.altinstr_replacement` 节长被吹大三倍 | ✅ **已修**（2026-10-25：实现 `.skip`/`.space` 与测距遍、GAS 关系运算为真给 -1、标号按「真身所在节」结算（`pushsection_label_offsets` 覆盖可执行区间）、可执行节追加只按节自己的对齐）。三层嵌套 `ALTERNATIVE` 的 `.altinstructions` 与 `.altinstr_replacement` 已做到与 gcc 逐字节一致（用例 `nested_alternative_entries_match_gas_byte_for_byte`） |
| 函数对齐（`-falign-functions=N` / `-fmin-function-alignment=N`） | 内核 `CONFIG_FUNCTION_ALIGNMENT=16` 靠它把每条函数摆到 16 字节边界，并顺带把 `.noinstr.text`/`.init.text`/`.entry.text` 的 `sh_addralign` 抬到 16 | ✅ **已实现**（2026-10-25：两个写法都解析并送进 `Target::align_functions`，发射与自定义节取同一口径）。包装器不再吞掉 `-fmin-function-alignment=*`，而是在交给 gcc 时重写成 `-falign-functions=*`——本机 gcc 13 认不得新写法，而内核的 `cc-option` 探测问的是包装器，吞掉会让探测谎报「支持」 |
| `~/.dcc/cache` 无限增长 | 全树单轮可涨到 38 GB / 44 万条目，且无目录环境变量与淘汰策略 | ⚠️ **未修**（本轮靠外部 `find ~/.dcc/cache -type f -mmin +30 -delete` 控住，已写进 §5.0；缓存是内容寻址，删任何条目只会多算不会错值，适合加容量上限淘汰） |

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
# 在本地 Linux 上编译 debug 版本
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
定位错误位置。`asm-offsets.c` 已于 2026-07-17 全链路通过。

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
- GCC 兼容性: `docs/roadmap.md`
