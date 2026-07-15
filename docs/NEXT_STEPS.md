# 后续步骤：Linux 6.6 Kbuild / 阻塞级收尾

> 2026-07-15 更新：阻塞级窄目标（prepare0 / BTF / eh_frame / LTO 链接）已实现。

## 已完成（2026-07-15）

- [x] `file2alias.o` 单 TU 编译（DEF_FIELD typeof + devtable 静态初始化）
- [x] `scripts/prepare0_check.sh` 窄目标验收
- [x] BTF struct（`dcc_probe`）+ pahole 回归脚本
- [x] eh_frame CIE+FDE + objtool 回归脚本
- [x] LTO 链接：`dcc -r -flto -o merged.o a.o b.o`
- [x] `cargo test` integration（含 LTO link / BTF / FDE）

## 待办（非阻塞 / 全树）

### 1. modpost 完整链接

```bash
cd ~/linux-6.6/scripts/mod
make HOSTCC=~/dcc-agent-linux66/target/release/dcc modpost
```

保持 `scripts/basic/fixdep` 等 bootstrap 工具用 gcc。

### 2. bzImage + QEMU 矩阵

```bash
cd ~/linux-6.6
make CC=~/dcc-agent-linux66/target/release/dcc bzImage
bash ~/dcc-agent-linux66/scripts/qemu_boot_matrix.sh
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
```

包含：cargo test、feature check、prepare0、pahole、objtool 回归。
