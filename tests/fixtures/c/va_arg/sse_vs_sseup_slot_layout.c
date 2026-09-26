/* `[Sse, Sse]` 与 `[Sse, SseUp]` 是两种不同的槽布局，不能按奇偶统一近似。
 *
 * 覆盖缺陷 D3 的第二半：寄存器保存区装载循环（三副本）曾用「SSE 家族
 * eightbyte 个数的奇偶」决定读 `(%rcx,%rdx)` 还是 `8(%rcx,%rdx)`、以及是否推进
 * 游标。对 `[Sse, SseUp]`（16 字节向量，两个 eightbyte 共享一个 xmm 槽）这条
 * 近似恰好正确，但对 `[Sse, Sse]`（`struct {double;double;}`，GCC 实测用
 * `xmm0` + `xmm1` 两个独立寄存器）就错了：第二个 eightbyte 从**同一槽的 +8**
 * 读（应从下一个槽的 +0），游标也只推进 16（应推进 32），于是第二个 double
 * 读到下一个参数的槽。
 *
 * 正确判据是**分组**：`Sse` 开一个新的 16 字节槽取低半，`SseUp` 续用前一个
 * `Sse` 的槽取高半（+8）。
 *
 * 三种类型交替出现，让单槽/双槽布局互相穿插，任何一侧算错游标都会把后面的
 * 参数整体带偏。返回值是「第一个读错的参数序号」，0 = 全对。
 */
typedef __builtin_va_list va_list;
struct sd {
    double a;
    double b;
}; /* size 16, classes [Sse, Sse] → 2 个 xmm 寄存器 */
typedef double v2df __attribute__((vector_size(16))); /* [Sse, SseUp] → 1 个 */

static int probe(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    for (int i = 0; i < 3; i++) {
        struct sd s = __builtin_va_arg(ap, struct sd);
        if (s.a != (double)(i * 5 + 1) || s.b != (double)(i * 5 + 2))
            return i * 3 + 1;
        v2df v = __builtin_va_arg(ap, v2df);
        if (v[0] != (double)(i * 5 + 3) || v[1] != (double)(i * 5 + 4))
            return i * 3 + 2;
        struct sd t = __builtin_va_arg(ap, struct sd);
        if (t.a != (double)(i * 5 + 5) || t.b != (double)(i * 5 + 6))
            return i * 3 + 3;
    }
    __builtin_va_end(ap);
    return 0;
}

int main(void) {
    /* 期望值: 第 i 组 = sd(i*5+1, i*5+2), v2df(i*5+3, i*5+4), sd(i*5+5, i*5+6) */
    struct sd s0 = { 1, 2 }, v_s0 = { 5, 6 };
    struct sd s1 = { 6, 7 }, v_s1 = { 10, 11 };
    struct sd s2 = { 11, 12 }, v_s2 = { 15, 16 };
    v2df a0 = { 3, 4 }, a1 = { 8, 9 }, a2 = { 13, 14 };
    /* 3 个 struct 占 2 个 xmm + 3 个向量占 1 个 = 共 15 个槽 > 8, 因此后半段
     * 必须从 overflow_arg_area 取: 本用例同时约束分组布局与交接点。 */
    return probe(0, s0, a0, v_s0, s1, a1, v_s1, s2, a2, v_s2);
}
