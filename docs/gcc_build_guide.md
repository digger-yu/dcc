# GCC 源码编译（短期目标）

> 最终目标仍是 Linux 内核；短期用 GCC 树中的**纯 C**子集（`libiberty`、
> `fixincludes` 等）暴露 dcc 与现代 glibc/GNU C 的差距。
> 最后更新：2026-08-20。
> 调试环境：本地 Ubuntu (已安装, 磁盘充足), 不再依赖远端 SSH.

## 范围说明

| 项 | 说明 |
|----|------|
| 目标 | 用 `dcc -c` 编译 GCC 源码树中仍以 C 为主的组件 |
| 首选子集 | `libiberty/*.c`（主机工具库，纯 C） |
| 扩展子集 | `fixincludes/*.c`（fixinc 工具链，纯 C） |
| 主体门禁 | `scripts/cc1plus_bootstrap.sh`（M26.6）：以 dcc 为 CC/CXX 配置并构建 cc1/cc1plus，产出进度报告跟踪 bootstrap 收敛（xfail 门禁） |
| 非目标（现阶段） | 完整 GCC bootstrap；GCC 主体为 **C++**，dcc 无 C++ 前端 |
| `libdecnumber` | 源文件为 **include 单元**，须按 DECPMAX 宏组合，单文件 `-c` 跳过 |
| 调试环境 | 本地 Ubuntu, 磁盘充足, `~/gcc-src` 稀疏克隆在本地 |

## cc1/cc1plus bootstrap 门禁

```bash
# 需要完整 GCC 源码树 (gcc/ 子目录), 建议全量克隆 (约 1-2GB 稀疏):
git clone --depth 1 https://github.com/gcc-mirror/gcc.git ~/gcc-src
cd ~/dcc && cargo build --release

# 运行门禁 (无源码树时 SKIP 退出 0):
GCC_SRC=~/gcc-src bash scripts/cc1plus_bootstrap.sh
# 退出码: 0 = SKIP 或 cc1/cc1plus 全量构建 (收敛); 2 = xfail (记录差距)
# 报告: $GCC_SRC/../dcc-bootstrap-build/cc1plus_progress.txt
#   (对象数 / 错误数 / 首批失败文件, 用于度量 dcc 对 GCC 主体的收敛)

# 通过 cargo 集成测试驱动 (需 --features gcc_src + GCC_SRC 环境变量):
GCC_SRC=~/gcc-src cargo test --release --features gcc_src \
  --test integration gcc_cc1plus_bootstrap_gate
```

## 本地探测

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
bash test_tr2.sh            # 最小例
bash test_v3_cache.sh       # 旧 V2 缓存存在时 regex.c 仍正确
python3 rootcause_sim.py
```

## 与内核路径的关系

- 内核路径：`docs/kernel_build_guide.md`、`docs/missing_features.md`
- GCC 路径：用更"脏"的用户态头（glibc `_Generic` / `include_next`）补洞
- 二者互补；**全树 GCC 仍受阻于 C++ 前端收敛度**

## 2026-08-29 GCC 15.3.0 编译进度

> 目标：用 dcc 编译完整 GCC 15.3.0（源码在 /root/gcc-15.3.0）

| 组件 | 状态 | 说明 |
|------|------|------|
| libiberty | ✅ 完成 | 纯 C，125 文件全部编译 |
| fixincludes | ✅ 完成 | 纯 C，6 文件全部编译 |
| libcpp | ✅ **14/14 完成** | 所有库文件编译成功，libcpp.a 已创建 |
| libcody | ✅ **8/8 完成** | GCC 15 新增的 C++ 模块支持库 |
| GCC 主体 | 进行中 | 生成器程序编译中 |

### libcpp 编译详情

libcpp 是 GCC 的预处理器库，包含 14 个 C++ 源文件。所有文件
均已成功编译并归档为 libcpp.a：

- charset.cc, directives.cc, errors.cc, expr.cc
- files.cc, identifiers.cc, init.cc, lex.cc
- line-map.cc, macro.cc, mkdeps.cc, pch.cc
- symtab.cc, traditional.cc

### 解析器错误跟踪修复

C++ 解析器的 parse_top_level() 之前会静默跳过无法解析的构造，
总是返回 Ok 和部分结果。这导致 fallback 路径（strip_cpp_keywords）
从未被使用，即使解析器只解析了文件的一小部分。

现在 parse_top_level() 在 parse_decl() 失败时记录错误诊断信息。
驱动模块检查错误诊断信息并在有错误时使用 fallback 路径，确保
整个文件都被处理。这修复了 genmodes.cc 等复杂 C++ 文件的不完整
编译问题。

### libcody 编译详情

libcody 是 GCC 15 新增的 C++ 库，用于模块支持。dcc 的 C++ 前端
成功降级了所有 8 个源文件：

- buffer.cc, client.cc, fatal.cc, netclient.cc
- netserver.cc, resolver.cc, packet.cc, server.cc

关键修复：
1. configure 脚本 C++11 检测：改为接受 C++14/17/20
2. 模板类型名 `::` 展平：`std::string` → `std_string`
3. 构造函数参数类型推断：不再全部用 `int` 占位
4. 重复 typedef 移除：`__fsid_t` 由系统头文件提供

### GCC 主编译器当前状态

GCC 主编译器构建需要先生成器程序（genmodes, genemit 等），
这些程序在主机上运行以生成头文件和源代码。

已完成：
- auto-host.h 类型宏冲突修复（ssize_t, pid_t 等）
- gcc/Makefile 编译器路径修正（CC/CXX 指向 dcc）
- errors.cc 生成器文件编译成功 ✅
- **genmodes.cc 编译成功** ✅ （关键生成器，包含 main 函数）
- strip_cpp_keywords 重大修复：
  - 控制流关键字（if/while/for/switch）不再被误识别为成员函数
  - if 条件变量声明正确转换为 C 风格
  - remove_all_default_args 不再破坏 if 条件中的初始化
  - 添加 std_swap 宏辅助函数

当前受阻：
- 链接生成器程序时出现 inline 函数多重定义错误
- 需要修复 C++ 前端对 inline 函数的处理
- 其他生成器程序（genemit, genrecog 等）待编译

### 构建命令

```bash
cd /root/gcc-15.3.0/build
make all-libcody    # ✅ 成功
make all-gcc        # 受阻于 libcpp
```
