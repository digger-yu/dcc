/* 内联 asm 的操作数落进 callee-saved 号时，那个号必须登记回寄存器占用的真源。
 * 本轮实测到两条通路都会因为它缺登记而静默错值（P12 Phase 6.1）：
 *
 *  1. **固定约束**（这里的 `"b"`/`"=b"` 类）：操作数直接写进 %rbx，而 `used_by_self`
 *     没置位 ⇒ 序言那条 `mov %rbx,-0x10(%r13)` 被 `patch_callee_saves` 改成 nop ⇒
 *     本函数毁掉**调用者**存在 %rbx 里的值。这与 §7.15 记的第一条错值同族，只是走的
 *     是 asm 那侧。判据：`caller_holds_rbx` 里的 `v` 跨过 `asm_add_b` 之后必须还是 1000。
 *  2. **池子只有九个 caller-saved 号**：操作数多于九个时旧实现会回退到「复用已经发出去
 *     的号」，于是第十个输出落在第一个的家上 —— 十个 `"=r"` 输出实测得 `s=64`（gcc 是 55）。
 *     现在池子按「先 caller-saved、不够再用 callee-saved」扩到十三个号，动到 callee-saved
 *     就立刻 `claim`（一次置好「序言要保存」与「溅出池别再发」两个位）。
 *
 * 反向对照 `six_q_outputs_still_distinct`：六个 `"=q"` 输出仍要各得其所（这条是 §7.15
 * 缺陷 3/4 修完留下的既有边界，不得被扩池改坏）。
 */
#include <stdio.h>

static long sink;

__attribute__((noinline)) static void touch(long v)
{
    sink = v;
}

/* 通路 1：固定约束 "b" 让操作数住进 %rbx。 */
__attribute__((noinline)) static int asm_add_b(int a, int b)
{
    int result;
    __asm__ __volatile__("addl %%ebx, %%eax\n\tmovl %%eax, %0"
                         : "=r"(result)
                         : "a"(a), "b"(b));
    return result;
}

__attribute__((noinline)) static long caller_holds_rbx(long a, long b)
{
    register long v asm("rbx");
    v = 1000;
    int r = asm_add_b((int)a, (int)b);
    touch(r);
    return (v == 1000) ? (long)r : -1;
}

/* 通路 2：十个 `"=r"` 输出，超过原来的九个 caller-saved 号。 */
__attribute__((noinline)) static long ten_outputs(void)
{
    long a = 0, b = 0, c = 0, d = 0, e = 0, f = 0, g = 0, h = 0, i = 0, j = 0;
    __asm__ volatile("movq $1, %0\n\tmovq $2, %1\n\tmovq $3, %2\n\tmovq $4, %3\n\t"
                     "movq $5, %4\n\tmovq $6, %5\n\tmovq $7, %6\n\tmovq $8, %7\n\t"
                     "movq $9, %8\n\tmovq $10, %9"
                     : "=r"(a), "=r"(b), "=r"(c), "=r"(d), "=r"(e), "=r"(f), "=r"(g),
                       "=r"(h), "=r"(i), "=r"(j));
    sink = a + b;
    return a + b + c + d + e + f + g + h + i + j; /* 55 */
}

/* 反向对照：legacy `q` 类六个输出仍各自独立。 */
__attribute__((noinline)) static long six_q_outputs(long *o0)
{
    long a = 0, b = 0, c = 0, d = 0, e = 0, f = 0;
    __asm__ volatile("movq $11, %0\n\tmovq $22, %1\n\tmovq $33, %2\n\t"
                     "movq $44, %3\n\tmovq $55, %4\n\tmovq $66, %5"
                     : "=q"(a), "=q"(b), "=q"(c), "=q"(d), "=q"(e), "=q"(f));
    *o0 = a;
    return a + b + c + d + e + f; /* 231 */
}

int main(void)
{
    long r1 = caller_holds_rbx(100, 200); /* 300，且调用者的 %rbx 必须还在 */
    long r2 = ten_outputs();               /* 55 */
    long q0 = 0;
    long r3 = six_q_outputs(&q0);          /* 231 */
    printf("r1=%ld r2=%ld r3=%ld q0=%ld sink=%ld\n", r1, r2, r3, q0, sink);
    return (r1 == 300 && r2 == 55 && r3 == 231 && q0 == 11) ? 0 : 1;
}
