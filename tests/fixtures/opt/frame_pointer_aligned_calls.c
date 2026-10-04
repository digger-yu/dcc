/* frame-pointer 档（`-fno-omit-frame-pointer`）的两条判据，都是本轮实测到的静默/崩溃：
 *
 *  1. **体内每个 call 点必须 rsp ≡ 0 (mod 16)**。序言在 FP 档推了两个寄存器（rbp 与 r13），
 *     而入口时 rsp ≡ 8，两次之后仍是 8 ⇒ 每个调用点都差 8 字节。实测同一份源在
 *     `-fno-omit-frame-pointer` 下 **SIGSEGV(139)**（printf 的浮点路径在 glibc 里用
 *     对齐 SSE 落栈），不带该 flag 时正常，gcc 两档都正常。
 *  2. **FDE 必须能区分 r13 与 rbp 的保存槽**。FP 档 CFA = r13+24（多压了一层 rbp），
 *     所以 r13 的 `DW_CFA_offset` 因子是 3；写死成 2 会让 r13 与 rbp 两条都指向 cfa-16，
 *     展开穿过 FP 帧时 r13 被恢复成 rbp 的值。判据在 tests/integration/opt_ssa_gate.rs
 *     里用 readelf 读产物，不靠肉眼。
 *
 * 本夹具刻意不用嵌套函数：那条路另有一个「-O2 并行 codegen 丢符号」的独立缺陷
 * （设计文档 §11.13 的 N1），混进来会让这条判据跑不到要点上。
 */
#include <stdio.h>

static double sink;

__attribute__((noinline)) static double leaf(double a, double b, double c, double d)
{
    sink = a + b + c + d;
    return (a + b) * 1.5 + (c + d) * 0.25;
}

__attribute__((noinline)) static double frame_chain(int n)
{
    double acc = 0.0;
    for (int i = 0; i < n; i++) {
        char buf[24];
        double v = leaf(i, i + 0.5, i + 1.0, i + 1.5);
        /* 这里既走调用（判据 1），也让帧里留着局部数组与 float 参数 */
        __builtin_snprintf(buf, sizeof buf, "%d", (int)v);
        acc += v + (double)buf[0] * 0.0;
        printf("i=%d v=%.2f acc=%.2f\n", i, v, acc);
    }
    return acc + sink;
}

int main(void)
{
    double r = frame_chain(3);
    printf("r=%.4f\n", r);
    /* 逐次：v = 1.375 / 4.875 / 8.375 ⇒ acc = 14.625；而 `sink` 是**覆盖写**，
     * 最后一次是 2+2.5+3+3.5 = 11.0 ⇒ r = 25.625（这个数与 gcc 逐字节相同）。 */
    return r > 25.6 && r < 25.7 ? 0 : 1;
}
