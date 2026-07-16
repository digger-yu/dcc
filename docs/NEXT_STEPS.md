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
- [x] 远程 libiberty 当前：**125 OK / 0 FAIL**（含 `regex.c`）

## 已完成（2026-07-16，内核全树脚本 + LTO gc）

- [x] `scripts/modpost_link.sh` — HOSTCC=dcc 完整链接 modpost（fixdep 仍用 gcc）
- [x] `scripts/qemu_boot_matrix.sh` + `scripts/kernel_full_check.sh` 编排（无 bzImage 时 SKIP）
- [x] `scripts/vmlinux_regress.sh` — `pahole -J` / `objtool check vmlinux`（无 vmlinux 时 SKIP）
- [x] LTO dead symbol：`-ffunction-sections` + `ld --gc-sections` + objcopy strip
- [x] 工作区回归脚本纳入 Git（`_*.sh` 调试脚本写入 `.gitignore`）
- [x] `sync_remote_build.sh` 接入 `kernel_full_check.sh`

## 待办（短期：GCC 纯 C 子集）

### 1. libiberty 后续（非阻塞）

`regex.c` 已通过：`substitute_params` 修复 + include 缓存 V3。
回归：

```bash
bash scripts/test_tr2.sh
bash scripts/test_v3_cache.sh
DCC=~/dcc/target/release/dcc GCCSRC=~/gcc-src \
  bash scripts/remote_libiberty_probe.sh
```

### 2. 明确不做完整 GCC bootstrap

GCC 主体为 C++；在具备 `dcc++` 之前，短期目标止于 **C 主机库 / gen\* 工具**。

## 待办（非阻塞 / 内核全树）

以下脚本与编译器能力已就绪；剩余工作是在远程 Linux 6.6 上跑通
`kernel_full_check.sh` / `BUILD_BZIMAGE=1` 并收集首份 PASS 日志：

- 全树 `make CC=dcc vmlinux` / `bzImage` 首次成功构建
- QEMU 矩阵在真实 bzImage 上非 SKIP 启动
- vmlinux 级 pahole/objtool 零 WARN

## 验证清单

```bash
bash scripts/sync_remote_build.sh
bash scripts/remote_libiberty_probe.sh   # 需已有 ~/gcc-src
bash scripts/kernel_full_check.sh      # modpost / QEMU / vmlinux（非阻塞）
```

包含：cargo test、feature check、prepare0、pahole、objtool、modpost 链接；以及可选的 libiberty 探测。
