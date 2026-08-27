// examples/dev/kernel_hwasan_tag_check.c
// -fsanitize=kernel-hwaddress: 内核模式 HWASAN 内联 tag 检查
//
// 编译: dcc -fsanitize=kernel-hwaddress -target aarch64-linux-gnu -c kernel_hwasan_tag_check.c
//
// 内核模式 HWASAN 不发 __hwasan_loadN/storeN 运行时调用,
// 改发内联 tag 检查代码:
//   1. 提取指针 tag: (addr >> 56) & 0xF
//   2. 读取内存 tag: *(u8*)((addr & 0x00FFFFFFFFFFFFFF) >> 4)
//   3. 比较: 不匹配调 __hwasan_tag_mismatchN(addr, size, is_write)
//
// 这与用户态 HWASAN (调 __hwasan_loadN/storeN) 和
// KASAN-kernel (shadow byte 检查) 均不同。

int x = 42;
int *p = &x;

// This should trigger inline tag check (not runtime call)
int y = *p;

// Store also gets inline tag check
*p = 100;

int main(void) {
    return y;
}
