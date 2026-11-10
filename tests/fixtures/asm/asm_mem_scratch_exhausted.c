/* 池耗尽且一个号都借不到时必须 **fail-closed**（报告 §5.43 #45）。
 *
 * 四个池号既被 `register asm` 钉成活值的家，又被同一条 asm 的 clobber 列表点名：
 * 没有「asm 之后还能还回去」的号可用，dcc 必须报 E0311 并拒绝出对象，而不是
 * 退回某个活值号静默给出错值。 */
struct S {
    unsigned long v;
};

register unsigned long g_a asm("rbx");
register unsigned long g_b asm("r12");
register unsigned long g_c asm("r14");
register unsigned long g_d asm("r15");

unsigned long reject_test(struct S *s, unsigned long x) {
    g_a = x + 1;
    g_b = x + 2;
    g_c = x + 3;
    g_d = x + 4;
    asm volatile("lock incq %0" : "+m"(s->v) : : "rbx", "r12", "r14", "r15");
    return g_a + g_b + g_c + g_d;
}
