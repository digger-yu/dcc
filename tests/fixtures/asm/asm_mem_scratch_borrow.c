/* 池耗尽时地址 scratch 必须**借号 + push/pop**（报告 §5.43 #45）。
 *
 * 四个文件作用域 `register asm` 把 rbx/r12/r14/r15 钉成全局变量的家；函数先给
 * 四个变量赋值，`"+m"(s->v)` 的地址在 asm 主体期间必须活着，而发号池四个号全被
 * 活值占着 —— 旧实现在这里兜底 `return 12`，静默复用持有活值的号，asm 结束后
 * 那个活值已经变成指针（内核现场：`__blk_mq_alloc_disk` 返回 `disk+0x160`）。
 *
 * 判据分两层：`-O2` 对象里 `push %rX … sub $0x8,%rsp`（对齐对）必须包住
 * `lock incq (%rX)` 并在之后成对弹出；四档运行语义与 gcc 一致。 */
struct S {
    unsigned long v;
};

register unsigned long g_a asm("rbx");
register unsigned long g_b asm("r12");
register unsigned long g_c asm("r14");
register unsigned long g_d asm("r15");

__attribute__((noinline))
unsigned long borrow_test(struct S *s, unsigned long x) {
    g_a = x + 1;
    g_b = x + 2;
    g_c = x + 3;
    g_d = x + 4;
    asm volatile("lock incq %0" : "+m"(s->v));
    return g_a + g_b + g_c + g_d + s->v;
}

int main(void) {
    struct S s = {41};
    /* (2+3+4+5) + 42 = 56；借号把某个活值踩掉就不是 56。 */
    return borrow_test(&s, 1) == 56 ? 0 : 1;
}
