# 跨平台支持

DCC 支持通过 `--target=<triple>` 或 `--target <triple>` 进行交叉编译，
跟 GCC/Clang/LLVM 行为一致。

## 三元组格式

```
<arch>-<vendor>-<os>-<abi>
```

vendor 可省略 (默认为 `unknown`)。

| arch | vendor | os | abi |
|------|--------|----|----|
| x86_64 | unknown / pc / apple | linux / windows / darwin / freestanding | gnu / musl / eabi / eabihf / msvc / darwin |
| aarch64 | unknown / apple | linux / darwin / freestanding | gnu / musl / darwin |
| riscv64 | unknown | linux / freestanding | gnu / musl |
| mips / mips64 | unknown | linux / freestanding | gnu / eabi |
| arm | unknown | linux / freestanding | gnueabi / gnueabihf / eabi |

## 使用示例

```bash
# 交叉编译到 aarch64
dcc --target=aarch64-linux-gnu -c -o hello.o hello.c

# 交叉编译到 arm (32-bit)
dcc --target=arm-linux-gnueabihf -c -o hello.o hello.c

# 交叉编译到 RISC-V 64
dcc --target=riscv64-linux-gnu -c -o hello.o hello.c

# 交叉编译到 MIPS (大端, 32-bit)
dcc --target=mips-linux-gnu -c -o hello.o hello.c

# 使用 sysroot 交叉编译 (musl)
dcc --target=aarch64-linux-musl --sysroot=/usr/aarch64-linux-gnu \
    -c -o hello.o hello.c

# 交叉编译 Linux 内核 (内核的 make 系统会读 CROSS_COMPILE)
CROSS_COMPILE=aarch64-linux-gnu- make ARCH=arm64 \
    CROSS_COMPILE=aarch64-linux-gnu- dcc
```

### Sysroot 自动搜索

交叉编译时若未显式指定 `--sysroot`, dcc 会自动搜索以下路径:
- `/usr/<target-triple>` (如 `/usr/aarch64-linux-gnu`)
- `/usr/local/<target-triple>`

若找到有效 sysroot 目录, verbose 模式 (`-v`) 会输出:
```
dcc: auto sysroot = /usr/aarch64-linux-gnu
```

## 当前支持的架构

| 架构 | 数据模型 | 字节序 | Codegen 状态 |
|------|----------|--------|-------------|
| x86_64 | LP64 | 小端 | ✅ 完整支持 (M0) |
| aarch64 | LP64 | 小端 | ✅ M7: hello/控制流/调用/浮点/向量/rodata |
| riscv64 | LP64 | 小端 | ✅ M7: hello/控制流/调用/浮点/向量/rodata |
| mips64 | LP64 | 大端 | ✅ 完整 codegen: 64-bit LD/SD + 帧布局适配 |
| mips | ILP32 | 大端 | ✅ 完整 codegen: 分支回填 + 全局变量 + switch |
| arm | ILP32 | 小端 | ✅ 完整 codegen: 分支回填 + 全局变量 + switch |

## 预定义宏差异

预处理器根据目标架构自动定义正确的宏, 不随宿主架构变化。

### 架构标识宏

| 架构 | 定义的宏 |
|------|----------|
| x86_64 | `__x86_64__`, `__amd64__`, `__k8__` |
| aarch64 | `__aarch64__`, `__ARM_ARCH=8`, `__ARM_NEON` |
| riscv64 | `__riscv`, `__riscv_xlen=64` |
| mips64 | `__mips__`, `__mips64`, `_MIPS_ISA=_MIPS_ISA_MIPS64` |
| mips | `__mips__`, `_MIPS_ISA=_MIPS_ISA_MIPS32` |
| arm | `__arm__`, `__ARM_ARCH=7` |

### 数据模型宏

| 模型 | 架构 | 定义的宏 |
|------|------|----------|
| LP64 | x86_64/aarch64/riscv64/mips64 | `__LP64__=1`, `__SIZEOF_LONG__=8`, `__SIZEOF_POINTER__=8` |
| ILP32 | mips/arm | `__ILP32__=1`, `__SIZEOF_LONG__=4`, `__SIZEOF_POINTER__=4` |

### 字节序宏

| 字节序 | 架构 | `__BYTE_ORDER__` |
|--------|------|------------------|
| 小端 | x86_64/aarch64/riscv64/arm | `__ORDER_LITTLE_ENDIAN__` |
| 大端 | mips/mips64 | `__ORDER_BIG_ENDIAN__` |

### x86 特有宏 (仅 x86_64 定义)

`__SSE2__`, `__SSE__`, `__MMX__`, `__CET__`, `__SEG_FS`, `__SEG_GS`

非 x86 架构不会定义这些宏。

## 已知限制

1. **DWARF debug info 完整发射路径** (`build_dwarf_full`) 目前仅在
   x86_64 后端执行。非 x86 后端有各自的简化 debug 路径。
2. **内置链接器** (ET_EXEC 直接可执行输出) 仅 x86_64 完整支持。
   交叉编译时建议使用 `-c` 生成 `.o` 文件, 再用目标平台的链接器链接。
3. **eh_frame / CFI** 完整发射仅 x86_64 支持。
4. **SSE/AVX 等 x86 特性标志** 在非 x86 目标上被静默忽略。
