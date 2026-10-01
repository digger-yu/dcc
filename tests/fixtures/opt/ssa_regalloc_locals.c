/* P12 自动寄存器分配的执行级夹具：把「热变量住进 callee-saved 寄存器」这件事的
 * 每一条判据都放在同一份程序里，任何一条判据错都会立刻变成错值。
 *
 * 为什么必须有这份夹具（而不是只靠 ssa_regalloc 的单测）：单测看的是分配器的计划，
 * 而计划对了、后端把变量放到寄存器后**读写路径**对不对是另一件事 —— §7.15~§7.19 修
 * 的就是这类「分配结果进了绑定表，但某个消费者没读表」的静默错值。这里刻意同时包含：
 *
 *   绑定的变量：循环累加器、循环归纳变量、跨函数调用仍然活着的变量、
 *              多个同时活着（必须分到不同号）的变量、生命期不相交（可以共用号）的变量；
 *   绝不能绑的变量：被取址的、volatile 的、static 的、浮点的、数组、结构体、
 *              只在某条路径上赋值的、内联汇编操作数、形参、被嵌套函数捕获的；
 *   以及一个用户手写 `register asm` 的变量，验证自动分配不会和它抢同一个号。
 *
 * 期望值全部取 gcc 实测（`assert_all_levels_match_gcc` 逐档对照），不用「 dcc 自己
 * 某一档的输出」当基准。
 */
#include <stdio.h>

static long gs[8];
static int fail;

__attribute__((noinline)) static long scale(long v, long k) { return v * k + 1; }

/* 被取址的变量必须留在内存里。 */
__attribute__((noinline)) static long escaped(long n) {
    long acc = 0;
    long *p = &acc;
    for (long i = 0; i < n; i++) acc += i;
    return *p + acc;
}

/* 只在一条路径上赋值 ⇒ 不绑（否则未初始化那条路径会读到调用者留在寄存器里的值）。 */
__attribute__((noinline)) static long cond_init(int c, long fallback) {
    long x;
    if (c) {
        x = 7;
    } else {
        x = fallback;
    }
    return x * 2;
}

/* 内联汇编的操作数 ⇒ 不绑（值必须还在内存/原来的位置上手能看见）。 */
__attribute__((noinline)) static long asm_operand(long n) {
    long v = n + 5;
    __asm__ volatile("addq $2, %0" : "+r"(v));
    long w = v * 2;
    __asm__ volatile("addq %1, %0" : "+r"(w) : "r"(v));
    return w;
}

/* 用户手写绑定占住 %rbx：自动分配必须让开这个号。 */
__attribute__((noinline)) static long user_bound(long n) {
    register long pinned asm("rbx");
    pinned = n + 3;
    long a = pinned * 2;
    long b = a + pinned;
    long c = b * 2;
    return c + pinned;
}

/* 多个同时活着的变量 + 生命期不相交的变量 + 跨调用的变量。 */
__attribute__((noinline)) static long mixed(long n) {
    long s1 = 0;
    long s2 = 1;
    long s3 = 2;
    for (long i = 0; i < n; i++) {
        s1 += i;
        s2 = scale(s2, 2) % 1000;
        s3 = s3 + s1 - i;
    }
    long early = s1 * 2;
    long mid = scale(early, 3);
    long late = mid + s2 + s3;
    return early + mid + late;
}

/* 浮点、数组、结构体、volatile 一律留在内存。
 *
 * 这里**没有**放 `static` 局部：在循环里写 static 局部会撞上另一条既存缺陷
 * （-O2 下 `static long st=4; for(...) st+=2;` 第二次调用读到 4 而不是 10，
 * gcc 是 10），那条债与分配无关，单独记在 docs/ssa-pass-upgrade-plan.md 里。 */
__attribute__((noinline)) static long kinds(long n) {
    volatile long vv = 3;
    double d = 1.5;
    long arr[4] = { 1, 2, 3, 4 };
    struct P { long x; int y; } p = { 5, 6 };
    for (long i = 0; i < n; i++) {
        d += 0.5;
        arr[i % 4] += i;
        p.x += i;
        p.y += 1;
        vv += 1;
    }
    long acc = (long) d + arr[0] + arr[1] + arr[2] + arr[3] + p.x + p.y + vv;
    /* 再来一层内块作用域：后端只会有一个家，这类名字必须不绑。 */
    {
        int acc2 = 9;
        acc += acc2;
    }
    return acc;
}

int main(void) {
    /* 期望值全部取 gcc 实测（各档位输出相同）。嵌套函数按帧偏移访问外层变量那条
     * 判据由 `ssa_regalloc::tests::function_with_nested_function_is_untouched` 钉住：
     * 这里不放嵌套函数，因为嵌套函数在 -O2 起另有既存缺陷（链接期
     * `undefined reference to inner`），会把两份债混成一条。 */
    gs[0] = escaped(6);
    gs[1] = cond_init(1, 11) + cond_init(0, 13);
    gs[2] = mixed(2);
    gs[3] = asm_operand(7);
    gs[4] = user_bound(5);
    gs[5] = mixed(5);
    gs[6] = kinds(3) + kinds(0);
    gs[7] = mixed(1) + kinds(1);
    printf("%ld %ld %ld %ld %ld %ld %ld %ld\n",
           gs[0], gs[1], gs[2], gs[3], gs[4], gs[5], gs[6], gs[7]);
    if (gs[0] != 30) fail = 1;   /* *p 与 acc 是同一块内存：15 + 15 */
    if (gs[1] != 40) fail = 2;   /* 7*2 + 13*2 */
    if (gs[2] != 25) fail = 3;   /* mixed(2)：同一函数另一个入参，绑定与不绑定必须一致 */
    if (gs[3] != 42) fail = 4;   /* v=12 → +2 = 14；w=28 → +v = 42 */
    if (gs[4] != 56) fail = 5;   /* pinned=8, a=16, b=24, c=48 → 48+8 */
    if (gs[5] != 217) fail = 6;  /* 三个同时活着的累加器 + 两次跨调用 */
    if (gs[6] != 82) fail = 7;   /* kinds(3)+kinds(0)：浮点/数组/结构体/volatile 都留在内存 */
    if (gs[7] != 44) fail = 8;   /* mixed(1)+kinds(1)：循环真的算了、两种路径一致 */
    return fail;
}
