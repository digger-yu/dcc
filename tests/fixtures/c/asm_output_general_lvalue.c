/* asm 的**寄存器类输出**绑到非 Ident 左值时必须仍然写回。
 *
 * `writeback_asm_operand` 的每个分支都以 `let Expr::Ident { .. } = expr else { return }`
 * 开头，于是 `a[i]` / `*p` / `s.f` 这类输出被**静默丢掉**：值留在临时寄存器里随后被覆盖，
 * 目标对象一直是 0。这是「把结果取到数组元素」的常见内核写法。实测（修前 = `13226ab`，
 * 四个档位全部一样）：
 *
 *     __asm__ volatile("movq $5, %0" : "=q"(a[1]));
 *     __asm__ volatile("movq $7, %0" : "=r"(a[2]));
 *     __asm__ volatile("movq $9, %0" : "=q"(gs[0]));     // 全局数组
 *     __asm__ volatile("movq $11, %0" : "=r"(*p));       // 解引用
 *     __asm__ volatile("movl $13, %0" : "=r"(s.b));      // 结构体字段
 *     __asm__ volatile("movsd %1, %0" : "=x"(d[1]) : "x"(one));   // SSE 输出
 *         dcc: a1=0 a2=0 gs0=0 a3=0 sb=0 d1=0
 *         gcc: a1=5 a2=7 gs0=9 a3=11 sb=13 d1=1.5
 *
 * SSE 那两行还连着另一条缺陷：`movsd` 在这张指令表里同时有「字符串搬移」与
 * 「SSE 标量双精度」两个意思，旧代码没有按操作数个数分开，于是
 * `movsd %xmm1, %xmm0` 被编成 `0xA5`（`rep movsd`），产出**一跑就段错误**的程序
 * （修前 `=x` 输出连 Ident 左值也崩；见 `asm_output_on_indexed_lvalue_is_written_back`）。
 *
 * 为什么放在 `tests/fixtures/c/`：这个目录是 `check_opt_equivalence.sh` 的语料，以后
 * 「暂存约定一改就只在某一档暴露」的形状回归也会被门禁拦住。
 *
 * 控制项：宽度 1/2/4/8 各一条、局部与全局两种基址、一条字符串 `movsl`（证明按操作数
 * 个数分开两个助记符之后，字符串形式仍然编得出来）、以及「同一条 asm 里两个输出」
 * （证明地址计算不会踩到还没写回的另一个输出）。
 */
#include <stdio.h>
#include <string.h>

static long gs[4];

struct S {
    long a;
    int b;
    unsigned char c;
};

__attribute__((noinline)) static long store_one(long q) { return q * 3; }

int main(void) {
    long a[4] = {0, 0, 0, 0};
    double d[2] = {0, 0};
    float f[2] = {0, 0};
    struct S s = {0, 0, 0};
    long *p = &a[3];
    long in = 5;
    double one = 1.5;
    float onef = 2.5f;

    __asm__ volatile("movq %1, %0" : "=q"(a[1]) : "r"(in) : "memory");   /* 5 */
    __asm__ volatile("movq $7, %0" : "=r"(a[2]));                        /* 7 */
    __asm__ volatile("movq $9, %0" : "=q"(gs[0]));                       /* 9 */
    __asm__ volatile("movq $11, %0" : "=r"(*p));                         /* 11 */
    __asm__ volatile("movl $13, %0" : "=r"(s.b));                        /* 13 */
    __asm__ volatile("movb $14, %0" : "=q"(s.c));                        /* 14 */
    __asm__ volatile("movsd %1, %0" : "=x"(d[1]) : "x"(one));            /* 1.5 */
    __asm__ volatile("movss %1, %0" : "=x"(f[1]) : "x"(onef));           /* 2.5 */

    /* 两个输出同时存在：地址计算不得踩掉另一个还没写回的输出。 */
    long x = 0, y = 0;
    __asm__ volatile("movq $21, %0\n\tmovq $22, %1" : "=r"(x), "=r"(y));
    a[0] = x + y;                                                        /* 43 */

    /* 字符串形式的 `movsl`（无操作数）必须仍然是 0xA5，不被 SSE 那条挤掉。 */
    char ss[8], sd[8];
    for (int i = 0; i < 8; i++) { ss[i] = (char)(i + 1); sd[i] = 0; }
    __asm__ volatile("cld\n\tmovsl" : : "S"(ss), "D"(sd) : "memory", "cc");

    long t = store_one(1);                                               /* 3 */
    printf("a0=%ld a1=%ld a2=%ld a3=%ld gs0=%ld sb=%d sc=%d d1=%.1f f1=%.1f ss=%d sd0=%d t=%ld\n",
           a[0], a[1], a[2], *p, gs[0], s.b, s.c, d[1], f[1], sd[3], sd[0], t);
    return (a[0] == 43 && a[1] == 5 && a[2] == 7 && *p == 11 && gs[0] == 9 &&
            s.b == 13 && s.c == 14 && d[1] == 1.5 && f[1] == 2.5f &&
            sd[0] == 1 && sd[3] == 4 && x == 21 && y == 22 && t == 3 &&
            memcmp(sd, "\1\2\3\4\0\0\0\0", 8) == 0) ? 0 : 1;
}
