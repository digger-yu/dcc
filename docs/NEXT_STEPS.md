# 后续步骤：Linux 6.6 Kbuild / GCC libiberty / 阻塞级收尾

> 2026-07-16 更新：内核窄目标已通过；新增 GCC `libiberty` 短期探测路径。

## 已完成（2026-07-15）

- [x] `file2alias.o` 单 TU 编译（DEF_FIELD typeof + devtable 静态初始化）
- [x] `scripts/prepare0_check.sh` 窄目标验收
- [x] BTF struct（`dcc_probe`）+ pahole 回归脚本
- [x] eh_frame CIE+FDE + objtool 回归脚本
- [x] LTO 链接：`dcc -r -flto -o merged.o a.o b.o`
- [x] `cargo test` integration（含 LTO link / BTF / FDE）

## 已完成（2026-07-16）

- [x] `#if` 十六进制字面量（`0x41` 不被拆成 ident）
- [x] `#include_next`（glibc `limits.h` → GCC `limits.h`）
- [x] 宏展开涂蓝继承（消除 `argv.c` 栈溢出）
- [x] `_Generic` + 三元控制表达式逗号边界
- [x] `scripts/remote_libiberty_probe.sh` + `docs/GCC_BUILD_GUIDE.md`
- [x] 远程 libiberty 基线：**90 OK / 39 FAIL**
- [x] 浮点 AndAnd/OrOr、宏参/字符串、括号函数名、嵌套 fptr、restrict 数组
- [x] `##` 二次替换、多声明符/`&arr[i]`、前向 struct、config.h 补全
- [x] 远程 libiberty 当前：**124 OK / 1 FAIL**（剩 `regex.c`）

## 待办（短期：GCC 纯 C 子集）

### 1. 修完 `regex.c`

完整文件中 `TRANSLATE(*d)` 被展成 `(**d)`，`PREFETCH()` 未展开；独立最小例可过。
需对照宏表与条件编译交互。

```bash
DCC=~/dcc/target/release/dcc GCCSRC=~/gcc-src \
  bash scripts/remote_libiberty_probe.sh
```

### 2. 明确不做完整 GCC bootstrap

GCC 主体为 C++；在具备 `dcc++` 之前，短期目标止于 **C 主机库 / gen\* 工具**。

## 待办（非阻塞 / 内核全树）

### 1. modpost 完整链接

```bash
cd ~/linux-6.6/scripts/mod
make HOSTCC=~/dcc/target/release/dcc modpost
```

保持 `scripts/basic/fixdep` 等 bootstrap 工具用 gcc。

### 2. bzImage + QEMU 矩阵

```bash
cd ~/linux-6.6
make CC=~/dcc/target/release/dcc bzImage
bash ~/dcc/scripts/qemu_boot_matrix.sh
```

无 bzImage 时矩阵脚本自动 SKIP。

### 3. vmlinux 级 pahole / objtool

完整内核链接后：

```bash
pahole -J vmlinux
objtool check vmlinux
```

### 4. LTO dead symbol 删除

`dead_local_functions` 已就绪；下一步在链接期配合 `--gc-sections` 或自定义节剔除。

## 验证清单

```bash
bash scripts/sync_remote_build.sh
bash scripts/remote_libiberty_probe.sh   # 需已有 ~/gcc-src
```

包含：cargo test、feature check、prepare0、pahole、objtool；以及可选的 libiberty 探测。
