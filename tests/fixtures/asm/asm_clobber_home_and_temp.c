/* 内联汇编的 clobber 是「这个号之后的内容不保证」的宣告：谁此刻正拿着值，谁就得被保住。
 * 本轮实测的两条静默错值都因为它两侧都不认这个宣告（P12 Phase 5.2a）：
 *   1. 自动分配把循环变量 `a` 的家定在 %rbx，跨一条 `::: "rbx"` ⇒ dcc -O2 得 632360，gcc 3050；
 *   2. `(q + r)` 的表达式溅出槽在 %rbx 上跨同一条 asm ⇒ dcc 四个档位都得 38885，gcc 25。
 *   3. 第三条同类（本轮补上）：clobber `%r13`（本后端的帧基址）不守护也不报错，
 *      -O0/-O2 都是 SIGSEGV(139)，而 gcc 同一源得 10。守它的机制与前两条完全同一个，
 *      区别只在「谁持有」除了池号（家/溅出槽）还要算上帧基址；%rsp 守不了（它就是
 *      守护自己的基座），所以那边是 E0318 报错，见 tests/integration/c_asm.rs。
 * 修法是「一处判据、两处读」：发射点前后各插一条保存/恢复
 * （`codegen::x86_64::asm::emit::asm_clobber_held_regs`），而 `asm goto` 那种「跳出去就不经过
 * 恢复」的形态在函数入口就把号从溅出池里拿掉（`ban_escaping_asm_regs` +
 * `ast_asm::escaping_asm_clobbers`，判据只写一份）。
 *
 * 反向对照 `home_stays_in_register`：判据不得退化成「看见 clobber 就不绑」—— 被守护的号
 * 仍然必须是家（tests/integration/c_asm.rs 按产物字节钉这一条）。
 */
#include <stdio.h>

static long sink[4];

__attribute__((noinline)) static long g(long z)
{
    sink[0] = z;
    return z + 1;
}

/* 缺陷 1：五个候选变量 + 一条 clobber %rbx 的普通 asm，热循环里的 `a` 被绑在 %rbx 上。 */
__attribute__((noinline)) static long home_across_clobber(long n)
{
    long a = 0, b = 0, c = 0, d = 0, e = 0;
    for (long i = 0; i < n; i++) {
        a += 1;
        b += 2;
        __asm__ volatile("movq $12345, %%rbx" ::: "rbx");
        c += 3;
        d += 4;
        e += a + b + c + d;
    }
    sink[1] = e;
    return a * 1 + b * 2 + c * 3 + d * 4 + e * 5; /* n=10 ⇒ 3050 */
}

/* 缺陷 2：表达式溅出槽跨同一条 asm（栈机路径，与优化档位无关，-O0 也坏）。 */
__attribute__((noinline)) static long temp_across_clobber(long q, long r, long s)
{
    long t = (q + r) * ({
        __asm__ volatile("movq $7777, %%rbx" ::: "rbx");
        g(s);
    });
    return t; /* (2+3)*(4+1) = 25 */
}

/* 逃逸形态：跳出去的那条路不经过恢复 ⇒ 这个号整个函数都不能再当临时值发出去。
 * 两条路径都要真走到：偶数次迭代走 asm 里的 `jmp`（跳过 `a += 7`），奇数次迭代走
 * fall-through（`a += 7` 执行）。只测一条路等于没测守护与让号各自的分工。 */
__attribute__((noinline)) static long escaping_goto(long n)
{
    long a = 0, b = 0, acc = 0;
    for (long i = 0; i < n; i++) {
        a += 3;
        b += 5;
        if (!(i & 1)) {
            __asm__ goto("jmp %l0" : : : "rbx" : skipped);
        }
        a += 7;
    skipped:
        acc += a + b;
    }
    sink[2] = acc;
    return acc + a * 2 + b; /* n=6 ⇒ acc=231, a=39, b=30 ⇒ 339 */
}

/* 反向对照：被守护的号仍然是家（值真的在 %rbx 里跑完整个循环）。 */
__attribute__((noinline)) static long home_stays_in_register(long n)
{
    long a = 0, b = 0;
    for (long i = 0; i < n; i++) {
        a += i;
        __asm__ volatile("movq $99, %%r14" ::: "r14");
        b += a;
    }
    return a + b; /* n=6 ⇒ a=15, b=35 ⇒ 50 */
}

/* 第三类主人：帧基址本身。全部局部量都是 `%r13+disp`，毁掉它就是毁掉整个帧——修前
 * -O0/-O2 都是 SIGSEGV(139)（同一条件在 gcc 里只是普通 callee-saved，得 10）。守护它
 * 不需新机制：同一个 push/pop 对就能把帧基址原样还回来。 */
__attribute__((noinline)) static long frame_base_under_clobber(long n)
{
    long a = n + 5; /* 住在 %r13 相对槽里 */
    __asm__ volatile("movq $99, %%r13" : : : "r13", "memory");
    return a + 1; /* n=4 ⇒ 10，少一道守护就读不到这个槽 */
}

int main(void)
{
    long x1 = home_across_clobber(10);
    long x2 = temp_across_clobber(2, 3, 4);
    long x3 = escaping_goto(6);
    long x4 = home_stays_in_register(6);
    long x5 = frame_base_under_clobber(4);
    printf("x1=%ld x2=%ld x3=%ld x4=%ld x5=%ld sink0=%ld sink1=%ld sink2=%ld\n",
           x1, x2, x3, x4, x5, sink[0], sink[1], sink[2]);
    return (x1 == 3050 && x2 == 25 && x3 == 339 && x4 == 50 && x5 == 10 &&
            sink[0] == 4 && sink[1] == 550 && sink[2] == 231) ? 0 : 1;
}
