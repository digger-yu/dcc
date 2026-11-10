/* §5.27: 内联汇编的 `"m"` 操作数在 IR 里必须按**地址**交付。
 *
 * 内核 `readl/writel` 是
 *     asm volatile("movl %0, %1" :: "r"(val), "m"(*(volatile u32 *)addr) : "memory");
 * SSA 往返曾经把 `*(volatile u32 *)addr` 的读**物化**成临时值 `_tN`，重建回 AST
 * 只剩 `Ident(_tN)`，发射侧按「Ident 的对象住在该变量帧槽」处理 ⇒ 那句 mov 写进
 * 栈上临时槽而不是 MMIO。于是 IOAPIC 的索引寄存器永远写不进去：
 * `IOAPIC[0] version 0 / GSI 0-0`、`nr_irqs 33`（gcc 是 32 / GSI 0-23 / 256），
 * 随后 `irq_startup` 在未激活的 irq 上 WARN 并 double fault。
 *
 * 失败码：1/2=指针解引用的写没落到对象上、3=读回错、4=volatile 指针参数、
 *         5=结构体成员、6=数组元素、7="+m" 读改写、8=两次写互不覆盖
 */
static unsigned int regs[4];
static struct { unsigned int a; unsigned int b; } mmio_dev = {0x11111111u, 0x22222222u};

static void wr32(unsigned int val, void *addr)
{
    asm volatile("movl %0, %1" :: "r"(val), "m"(*(volatile unsigned int *)addr) : "memory");
}

static unsigned int rd32(const void *addr)
{
    unsigned int v;
    asm volatile("movl %1, %0" : "=r"(v) : "m"(*(const volatile unsigned int *)addr));
    return v;
}

static void wr32_vol(unsigned int val, volatile unsigned int *p)
{
    asm volatile("movl %0, %1" :: "r"(val), "m"(*p) : "memory");
}

int main(void)
{
    wr32(0x11223344u, &regs[0]);
    if (regs[0] != 0x11223344u) return 1;

    wr32(0x55667788u, &regs[1]);
    if (regs[1] != 0x55667788u) return 2;
    if (regs[0] != 0x11223344u) return 8;

    if (rd32(&regs[0]) != 0x11223344u) return 3;

    wr32_vol(0xA5A5A5A5u, &regs[2]);
    if (regs[2] != 0xA5A5A5A5u) return 4;

    asm volatile("movl %0, %1" :: "r"(0x0BADF00Du), "m"(mmio_dev.b) : "memory");
    if (mmio_dev.b != 0x0BADF00Du) return 5;

    asm volatile("movl %0, %1" :: "r"(0xFEEDFACEu), "m"(regs[3]) : "memory");
    if (regs[3] != 0xFEEDFACEu) return 6;

    asm volatile("addl $3, %0" : "+m"(*(volatile unsigned int *)&regs[1]));
    if (regs[1] != 0x55667788u + 3u) return 7;

    return 0;
}
