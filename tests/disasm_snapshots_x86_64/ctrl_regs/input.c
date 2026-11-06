/* Control/debug register moves (0F 20/21/22/23).
 *
 * ModRM 口径: reg 字段 = CRn/DRn, r/m 字段 = 通用寄存器。
 * 半边写反时 `mov %cr4,%rax` 会变成 `mov %cr0,%rsp` —— 反汇编文本层
 * 立刻能看出来, 但执行层看不出来 (内核 PVH 启动即 triple fault)。
 */
unsigned long read_cr4(void) {
    unsigned long v;
    __asm__ volatile("mov %%cr4,%0" : "=r"(v));
    return v;
}

void write_cr3(unsigned long v) {
    __asm__ volatile("mov %0,%%cr3" : : "r"(v) : "memory");
}

unsigned long read_dr7(void) {
    unsigned long v;
    __asm__ volatile("mov %%dr7,%0" : "=r"(v));
    return v;
}
