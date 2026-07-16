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

脚本会写入/刷新最小 `libiberty/config.h`（含 `HAVE_SYS_TYPES_H`、`#include <sys/types.h>`），
跳过非 Linux 平台桩，并汇总 OK/FAIL/SKIP。

## 2026-07-16 远程结果（当前）

| 指标 | 数值 |
|------|------|
| 可编译 `.c` | **124** |
| 失败 | **1**（`regex.c`） |
| 跳过 | **4**（`msdos` / `pex-djgpp` / `pex-msdos` / `pex-win32`） |
| 基线（本轮初） | 约 90 OK / 39 FAIL |

### 本轮已修（相对基线）

| 问题 | 根因 | 修复 |
|------|------|------|
| 浮点 `&&`/`\|\|` | 比较结果被标成 float，IR 拒 AndAnd | 比较/`&&`/`\|\|` 结果恒为 int |
| texinfo `@deftypefn` | 多被误判；实为缺 `HAVE_CONFIG_H` | 探测脚本加 `-DHAVE_CONFIG_H` |
| `strerror` ENTRY 宏 | 参数替换进字符串字面量 | `replace_ident` 跳过字符串/字符常量 |
| `htab_size` | `size_t (name)(params)` 括号防宏 | 支持括号包函数名 |
| 嵌套函数指针参数 | `char *(*pfn)(...)` | `parse_params` 指针后再认 `(*` |
| `[__restrict]` | 数组限定符当表达式 | 跳过 restrict/static/const |
| `&randtbl[i]` 全局初值 | 非常量 | `is_static_initializer` + rela addend |
| `&&di` 宏展开 | `##` 路径全局替换后再替换 | `handle_paste` 只替换 ## 两侧 |
| `struct T *a, *b` | `parse_type` 吃掉第一个 `*` | 说明符不含 `*`；多声明符循环 |
| `struct T *a, *b` 初值 | `parse_expr` 吞逗号 | 初值用 `parse_expr_bp(2)` |
| `(*f())` / `(*fns[N])()` | 返回指针的函数、函数指针数组 | 声明符路径补全 |
| `operators[][2]` | 只认一维数组 | 多维 `[]` 循环 |
| `__alignof__` | 未作 KwAlignof | token 识别 |
| `label: case` | label 后不能直接 case | 空语句标签 |
| 前向 struct 成员访问 | 空字段 Rc 不更新 | 空字段时查 `struct_defs` |
| `pid_t` / md5_uint32 | 缺 sys/types、stdint 路径 | config.h 补全 + `__INT_MAX__` 等 |
| 无参函数宏 `PREFETCH()` | `()` 被解析成一个空实参 | 空 `()` → 零个实参 |

### 仍失败

| 文件 | 现象 | 说明 |
|------|------|------|
| `regex.c` | `TRANSLATE(*d)` 展开成 `(**d)`；`PREFETCH()` 未展开 | 仅在完整 `regex.c` 上下文复现；独立最小例可过。待查宏表/条件编译交互 |

## 与内核路径的关系

- 内核路径：`docs/KERNEL_BUILD_GUIDE.md`、`docs/MISSING_FEATURES.md`
- GCC 路径：用更“脏”的用户态头（glibc `_Generic` / `include_next`）补洞
- 二者互补；**全树 GCC 仍受阻于无 C++**
