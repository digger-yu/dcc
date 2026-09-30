/* asm 的**寄存器类输出**绑到非 Ident 左值时必须仍然写回。
 *
 * `writeback_asm_operand` 的每个分支都以 `let Expr::Ident { .. } = expr else { return }`
 * 开头，于是 `a[i]` / `*p` / `s.f` 这类输出被**静默丢掉**：值留在临时寄存器里然后被覆盖，
 * 目标对象一直是 0。这是「把结果取到数组元素」的常见内核写法，实测（修前 = 13226ab）：
 *
 *     __asm__ volatile("movq $5, %0" : "=q"(a[1]));
 *     __asm__ volatile("movq $7, %0" : "=r"(a[2]));
 *     __asm__ volatile("movq $9, %0" : "=q"(gs[0]));    // 全局数组
 *     __asm__ volatile("movq $11, %0" : "=r"(*p));      // 解引用
 *     __asm__ volatile("movq $13, %0" : "=r"(s.b));     // 结构体字段
 *     dcc -O0/-O1/-O2/-O3: a1=0 a2=0 gs0=0 a3=0 sb=0 sc=0   （四个档位都一样）
 *     gcc                : a1=5 a2=7 gs0=9 a3=11 sb=13 sc=14
 *
 * 为什么放在 `tests/fixtures/c/`：`check_opt_equivalence.sh` 会扫这个目录，于是
 * 「-O0 与 -O2 行为不同」这类形状相关缺陷以后也会被门禁拦住（本例修前 -O0 与 -O2
 * 都是错的，但同一份代码里的暂存约定一改就会只在某一档暴露）。
 *
 * 覆盖三种宽度（1/4/8 字节）与两种基址（局部数组、全局数组、指针、字段），外加
 * 「输出与输入同时存在」的一条，确保地址计算不会踩到还没写回的其他输出。
 */
#include <stdio.h>

static long gs[4];

struct S {
    long a;
    int b;
    unsigned char c;
};

__attribute__((noinline)) static long store_one(long q) { return q * 3; }

int main(void) {
    long a[4] = {0, 0, 0, 0};
    struct S s = {0, 0, 0};
    long *p = &a[3];
    long in = 5;

    __asm__ volatile("movq %1, %0" : "=q"(a[1]) : "r"(in) : "memory");   /* 5 */
    __asm__ volatile("movq $7, %0" : "=r"(a[2]));                         /* 7 */
    __asm__ volatile("movq $9, %0" : "=q"(gs[0]));                        /* 9 */
    __asm__ volatile("movq $11, %0" : "=r"(*p));                          /* 11 */
    __asm__ volatile("movl $13, %0" : "=r"(s.b));                         /* 13 */
    __asm__ volatile("movb $14, %0" : "=q"(s.c));                         /* 14 */

    /* 两个输出同时存在：地址计算不得踩掉另一个还没写回的输出。 */
    long x = 0, y = 0;
    __asm__ volatile("movq $21, %0\n\tmovq $22, %1" : "=r"(x), "=r"(y));
    a[0] = x + y;                                                         /* 43 */

    long t = store_one(1);                                                /* 3 */
    printf("a0=%ld a1=%ld a2=%ld a3=%ld gs0=%ld sb=%d sc=%d x=%ld y=%ld t=%ld\n",
           a[0], a[1], a[2], *p, gs[0], s.b, s.c, x, y, t);
    return (a[0] == 43 && a[1] == 5 && a[2] == 7 && *p == 11 && gs[0] == 9 && s.b == 13 &&
            s.c == 14 && x == 21 && y == 22 && t == 3) ? 0 : 1;
}
