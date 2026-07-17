# 跨平台支持

DCC 支持通过 `--target=<triple>` 进行跨平台编译，跟 GCC/Clang/LLVM 行为一致。

## 三元组格式

```
<arch>-<vendor>-<os>-<abi>
```

| arch | vendor | os | abi |
|------|--------|----|----|
| x86_64 | unknown / pc / apple | linux / windows / darwin / freestanding | gnu / musl / eabi / eabihf / msvc / darwin |
| aarch64 | unknown / apple | linux / darwin / freestanding | gnu / musl / darwin |
| riscv64 | unknown | linux / freestanding | gnu / musl |
| mips / mips64 | unknown | linux / freestanding | gnu / eabi |
| arm | unknown | linux / freestanding | gnueabi / gnueabihf / eabi |

## 工具链命名

跟 GNU 工具链一致，源文件名前缀：

```
<arch>-<vendor>-<os>-<abi>-dcc        # 编译器
<arch>-<vendor>-<os>-<abi>-dcc-cc     # 编译器别名
```

例子:
- `x86_64-unknown-linux-gnu-dcc`
- `aarch64-linux-gnu-dcc`
- `riscv64-linux-gnu-dcc`
- `arm-linux-gnueabihf-dcc`

## 命令

```bash
# 编译到 aarch64
dcc --target=aarch64-linux-gnu -o hello-arm64 hello.c

# 交叉编译 Linux 内核 (内核的 make 系统会读 CROSS_COMPILE)
CROSS_COMPILE=aarch64-linux-gnu- make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- dcc
```

## 当前支持的架构

| 架构 | 状态 |
|------|------|
| x86_64 | ✅ 完整支持 (M0) |
| aarch64 | ✅ M7：hello/控制流/调用/浮点/向量/rodata |
| riscv64 | ✅ M7：hello/控制流/调用/浮点/向量/rodata |
| mips / mips64 | ⏳ M7 之后 |
| arm | ⏳ M7 之后 |
