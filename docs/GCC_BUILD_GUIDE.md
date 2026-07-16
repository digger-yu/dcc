# GCC 源码编译（短期目标）

> 最终目标仍是 Linux 内核；短期用 GCC 树中的**纯 C**子集（`libiberty`）
> 暴露 dcc 与现代 glibc/GNU C 的差距。最后更新：2026-07-16。

## 范围说明

| 项 | 说明 |
|----|------|
| 目标 | 用 `dcc -c` 编译 GCC 源码树中仍以 C 为主的组件 |
| 首选子集 | `libiberty/*.c`（主机工具库，纯 C） |
| 非目标（现阶段） | 完整 GCC bootstrap；GCC 主体为 **C++**，dcc 无 C++ 前端 |
| 远程 | `ssh ds@192.168.148.128`，源码稀疏克隆于 `~/gcc-src` |

## 远程探测

```bash
# 稀疏克隆（约数十 MB，勿拉全树）
git clone --depth 1 --filter=blob:none --sparse \
  https://github.com/gcc-mirror/gcc.git ~/gcc-src
cd ~/gcc-src && git sparse-checkout set libiberty include

# 构建 dcc 并探测
cd ~/dcc && cargo build --release
DCC=~/dcc/target/release/dcc GCCSRC=~/gcc-src \
  bash scripts/remote_libiberty_probe.sh
```

脚本会写入最小 `libiberty/config.h`（未跑 autoconf 时），并汇总 OK/FAIL。

## 2026-07-16 远程结果（基线）

| 指标 | 数值 |
|------|------|
| 可编译 `.c` | **90** |
| 失败 | **39** |
| 此前阻塞 | 栈溢出 / `#include_next` / `#if 0x..` / `_Generic`+三元 |

### 本轮已修（为编 GCC/glibc 头所触发）

| 问题 | 根因 | 修复 |
|------|------|------|
| `#if 'A' == 0x41` 恒假 | `replace_unknown_idents` 把 `0x41` 拆成 `0`+`x41`→`00` | 数字字面量整段保留 |
| `safe-ctype.h` HOST_CHARSET | 同上 | 同上 |
| 编译 `argv.c` 栈溢出 | 函数宏参数预展开用空 disabled 集，嵌套宏无限递归 | 参数展开继承涂蓝集 + 深度保护 |
| `<limits.h>` E0204 | `#include_next` 被当成 `#include` | 实现 `#include_next` |
| glibc `_Generic(a?b:c, …)` | 三元 else 用 `parse_expr_bp(0)` 吞掉逗号 | else 用 bp=2，并尊重 min_bp |

### 仍失败的典型类别（后续）

| 类别 | 示例文件 | 说明 |
|------|----------|------|
| 浮点逻辑 | `copysign.c` | SSE 路径缺 `AndAnd`/`OrOr` |
| C++ demangle | `cp-demangle.c` 等 | 复杂 GNU C / 设计器语法 |
| 平台桩 | `pex-djgpp.c` / `msdos.c` | 非 Linux 头，可跳过 |
| texinfo 注释泄漏 | `filename_cmp.c` 等 | `@deftypefn` 块被当代码 |
| 其余 | `obstack` / `regex` / `simple-object*` | 宏与类型尚未对齐 |

## 与内核路径的关系

- 内核路径：`docs/KERNEL_BUILD_GUIDE.md`、`docs/MISSING_FEATURES.md`
- GCC 路径：用更“脏”的用户态头（glibc `_Generic` / `include_next`）补洞
- 二者互补；**全树 GCC 仍受阻于无 C++**
