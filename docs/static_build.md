# DCC 静态链接构建指南

本文档说明如何将 dcc 编译器构建为完全静态链接的二进制文件，使其不依赖 host 的动态链接器（`ld.so`）。

## 背景

默认情况下，dcc 二进制文件动态链接到 glibc，依赖 host 的 `ld.so` 进行运行时加载。在某些场景下（如跨系统部署、容器环境、嵌入式系统），需要完全静态的二进制文件。

通过 Rust 的 musl 目标 + `+crt-static` 特性，可以将 dcc 编译为单文件静态二进制，无需任何动态库依赖。

## 前置条件

### 1. 安装 musl 工具链

**Debian/Ubuntu:**
```bash
sudo apt install musl-tools
```

**Fedora:**
```bash
sudo dnf install musl-tools
```

**Arch Linux:**
```bash
sudo pacman -S musl
```

### 2. 安装 Rust musl 目标

```bash
# x86_64
rustup target add x86_64-unknown-linux-musl

# aarch64 (交叉编译)
rustup target add aarch64-unknown-linux-musl

# riscv64 (交叉编译)
rustup target add riscv64gc-unknown-linux-musl
```

## 构建方法

### 方法 1: 使用构建脚本（推荐）

```bash
# Release 构建（默认）
./scripts/build_static.sh

# Debug 构建
./scripts/build_static.sh debug
```

脚本会自动检测架构、安装 musl 目标（如未安装）、构建并验证静态链接。

### 方法 2: 手动构建

```bash
# 配置 cargo 使用静态链接（已在 .cargo/config.toml 中配置）
# [target.x86_64-unknown-linux-musl]
# rustflags = ["-C", "target-feature=+crt-static"]

# 构建
cargo build --release --target x86_64-unknown-linux-musl

# 输出位置
ls -lh target/x86_64-unknown-linux-musl/release/dcc
```

## 验证静态链接

构建完成后，验证二进制文件确实是静态链接的：

```bash
# 检查文件类型
file target/x86_64-unknown-linux-musl/release/dcc
# 应显示: "statically linked"

# 检查动态依赖
ldd target/x86_64-unknown-linux-musl/release/dcc
# 应显示: "not a dynamic executable" 或类似信息
```

## 支持的目标架构

| 架构 | Rust Target | 状态 |
|------|-------------|------|
| x86_64 | `x86_64-unknown-linux-musl` | ✅ 完整支持 |
| aarch64 | `aarch64-unknown-linux-musl` | ✅ 支持 |
| riscv64 | `riscv64gc-unknown-linux-musl` | ✅ 支持 |

## 与交叉编译的区别

**静态链接构建**（本文档）：
- 目标：使 dcc **本身**不依赖 host ld.so
- 用途：部署到不同 Linux 发行版、容器环境
- 命令：`cargo build --target x86_64-unknown-linux-musl`

**dcc 交叉编译**（见 `docs/cross_compile.md`）：
- 目标：dcc **编译出的代码**运行在不同架构
- 用途：为 ARM/RISC-V 等架构生成二进制
- 命令：`dcc --target=aarch64-linux-gnu -o hello-arm hello.c`

两者可以组合使用：先静态构建 dcc，再用该 dcc 交叉编译其他架构的代码。

## 故障排除

### 问题 1: musl 目标未安装

```
error[E0463]: can't find crate for `std`
```

**解决:**
```bash
rustup target add x86_64-unknown-linux-musl
```

### 问题 2: 链接器找不到 musl-gcc

```
error: linker `musl-gcc` not found
```

**解决:**
```bash
# Debian/Ubuntu
sudo apt install musl-tools

# 或创建符号链接
sudo ln -s /usr/bin/musl-gcc /usr/local/bin/
```

### 问题 3: 构建后仍有动态依赖

检查 `.cargo/config.toml` 是否正确配置：

```toml
[target.x86_64-unknown-linux-musl]
rustflags = ["-C", "target-feature=+crt-static"]
```

确保使用 `--target x86_64-unknown-linux-musl` 而非默认目标。

## 性能影响

静态链接的二进制文件：
- **体积**: 略大（包含所有依赖）
- **启动速度**: 略快（无需动态链接开销）
- **运行性能**: 与动态链接版本相同

## CI 集成

在 GitHub Actions 或其他 CI 环境中：

```yaml
- name: Install musl tools
  run: sudo apt install musl-tools

- name: Install musl target
  run: rustup target add x86_64-unknown-linux-musl

- name: Build static binary
  run: ./scripts/build_static.sh release

- name: Upload artifact
  uses: actions/upload-artifact@v3
  with:
    name: dcc-static
    path: target/x86_64-unknown-linux-musl/release/dcc
```

## 相关文档

- [交叉编译指南](cross_compile.md) - dcc 交叉编译到其他架构
- [架构文档](architecture.md) - 整体架构设计
- [CI 指南](ci_guide.md) - 持续集成配置
