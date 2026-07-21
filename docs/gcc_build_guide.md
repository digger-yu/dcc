# GCC 源码编译（短期目标）

> 最终目标仍是 Linux 内核；短期用 GCC 树中的**纯 C**子集（`libiberty`、
> `fixincludes` 等）暴露 dcc 与现代 glibc/GNU C 的差距。
> 最后更新：2026-07-16。

## 范围说明

| 项 | 说明 |
|----|------|
| 目标 | 用 `dcc -c` 编译 GCC 源码树中仍以 C 为主的组件 |
| 首选子集 | `libiberty/*.c`（主机工具库，纯 C） |
| 扩展子集 | `fixincludes/*.c`（fixinc 工具链，纯 C） |
| 非目标（现阶段） | 完整 GCC bootstrap；GCC 主体为 **C++**，dcc 无 C++ 前端 |
| `libdecnumber` | 源文件为 **include 单元**，须按 DECPMAX 宏组合，单文件 `-c` 跳过 |
| 远程 | `ssh ds@192.168.148.128`，源码稀疏克隆于 `~/gcc-src` |

## 远程探测

```bash
# 稀疏克隆（约数十 MB，勿拉全树）
git clone --depth 1 --filter=blob:none --sparse \
  https://github.com/gcc-mirror/gcc.git ~/gcc-src
cd ~/gcc-src && git sparse-checkout set libiberty include fixincludes libdecnumber

# 构建 dcc 并探测
cd ~/dcc && cargo build --release
# 通过 `cargo test --test integration` 验证 libiberty 等 GCC 子集
# (libiberty 单 TU 测试与 libiberty `dcc -r` 链接都在 integration 套件内)
```

`scripts/libiberty_combined.sh`（由本地 cargo test 驱动）会写入/刷新最小
`libiberty/config.h`，汇总 libiberty 单文件编译、**整库 `dcc -r` 链接**、
fixincludes 与 libdecnumber（include-only 模块 SKIP）。

## 2026-07-16 本地结果（当前）

| 组件 | OK | FAIL | SKIP | 说明 |
|------|-----|------|------|------|
| libiberty | **125** | **0** | 4 | 非 Linux 平台桩 |
| libiberty 链接 | ✅ | — | — | `dcc -r` 合并 ~1MB `.o` |
| fixincludes | **6** | **0** | 1 | `fixtests` 为测试 harness |
| libdecnumber | 0 | 0 | 11 | include 单元，非单 TU |

### 本轮已修（2026-07-16 晚）

| 问题 | 根因 | 修复 |
|------|------|------|
| `char const *p` 解析失败 | `const` 在 `*` 前未跳过 | `finish_type` / 声明符路径先跳过限定符再 `parse_ptr_suffix` |
| fixincludes `fixincl`/`fixopts` | 同上 + `char const **` | 同上 |
| libiberty 链接 multiple definition | header `extern` + TU 定义重复进 symtab | 符号收集跳过 extern+定义重复 |
| `ld -r --gc-sections` 失败 | bfd 要求 `-u` 根符号 | `lto::gc_sections_root` + `link.rs -u` |

### 宏展开回归

```bash
bash scripts/archive/test_tr2.sh            # 最小例
bash scripts/archive/test_v3_cache.sh       # 旧 V2 缓存存在时 regex.c 仍正确
python3 scripts/archive/rootcause_sim.py
```

## 与内核路径的关系

- 内核路径：`docs/kernel_build_guide.md`、`docs/missing_features.md`
- GCC 路径：用更“脏”的用户态头（glibc `_Generic` / `include_next`）补洞
- 二者互补；**全树 GCC 仍受阻于无 C++**
