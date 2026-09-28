// examples/dev/kasan_kernel_multiarch.c
// -fsanitize=kernel-address: KASAN-kernel 架构感知 shadow 偏移
//
// 编译 (x86_64):     dcc -fsanitize=kernel-address -c kasan_kernel_multiarch.c
// 编译 (aarch64):    dcc -fsanitize=kernel-address -target aarch64-linux-gnu -c kasan_kernel_multiarch.c
// 编译 (riscv64):    dcc -fsanitize=kernel-address -target riscv64-linux-gnu -c kasan_kernel_multiarch.c
//
// 不同架构使用不同的 KASAN shadow 基址偏移:
//   x86_64:    shadow = (addr >> 3) + 0xdffffc0000000000
//   AArch64:   shadow = (addr >> 3) + 0xdfff600000000000  (VA_BITS=48)
//   RISC-V 64: shadow = (addr >> 3) + 0xdffffe0000000000  (SV39/SV48)
//
// 内联 shadow 检查不调用户态运行时 (__asan_loadN/storeN),
// 直接在代码中计算 shadow 地址并检查 shadow 字节。

int buf[16];

int read_elem(int idx) {
    // KASAN-kernel inline shadow check:
    // shadow = (addr >> 3) + arch_offset
    // 逐 granule 检查 shadow 字节是否毒化
    return buf[idx];
}

void write_elem(int idx, int val) {
    buf[idx] = val;
}

int main(void) {
    write_elem(0, 42);
    return read_elem(0);
}
