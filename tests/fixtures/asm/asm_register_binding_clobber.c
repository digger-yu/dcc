/* `register asm` 绑定的寄存器是向调用者「借」的：借之前必须保存，借期间别人
 * 不能再把它当临时值发出去。
 *
 * 三条实测缺陷全部出现在 -O0/-O1，而 -O2/-O3 恰好躲过 —— SSA 往返换了表达式
 * 形状，没走到那条溅出路径。所以这份夹具必须逐档与 gcc 对照：只看默认档等于
 * 没测，只看 -O2 会以为它一直是对的。
 *
 *  1. 被调方绑了 %rbx 却不保存它。序言里那四条 callee-saved 保存是先无条件
 *     发射、函数体生成完后由 `patch_callee_saves` 把「本函数没用到」的改成 nop;
 *     绑定路径从没登记过 `callee_saved_used`，于是保存被改成 nop，调用方跨调用
 *     存在 %rbx 里的值就此消失。修前实测（`caller_holds_rbx(10,20,30)`）：
 *         dcc -O0: r=1500     gcc: r=500
 *     file-scope 的绑定是另一条路：`compile_func` 开头就把绑定写进表，而位图在
 *     其后被 `reset_reg_tracker()` 清零，所以必须在那里重新登记。三层嵌套二元式
 *     依次占住 %rbx/%r12/%r14，最内层的调用毁掉 %r14。
 *  2. 同一函数里的表达式溅出池不知道 %rbx 已经是某个变量的家，把它当临时值发了
 *     出去，直接盖掉变量的家。修前实测（`v` 应为 7）：
 *         dcc -O0: x1=50 v0=25    gcc: x1=32 v0=7
 *
 * 反向对照 `pinned()`：不逸出、不冲突时绑定必须仍然生效（值真的落在 %rbx 里），
 * 判据不能退化成「把 register asm 整个特性关掉」。
 */
#include <stdio.h>

static long sink[4];
register long gv asm("r14");

__attribute__((noinline)) static long g(long z) { sink[3] = z; return z + 1; }

/* 缺陷 2：本函数自己的表达式溅出把变量的家盖掉了。 */
__attribute__((noinline)) static long spill_clobbers_home(long q, long r, long s) {
    register long v asm("rbx") = 7;
    long t = (q + r) * g(s);
    sink[0] = v;
    return t + v;
}

/* 缺陷 1：被调方只用绑定寄存器、不做任何二元式，于是它「没用过」%rbx —— 恰恰是
 * 这种函数最需要保存，因为值是被调用方留在 %rbx 里的。 */
__attribute__((noinline)) static long bind_only(long q) {
    register long v asm("rbx") = q;
    sink[1] = v;
    return v;
}
__attribute__((noinline)) static long caller_holds_rbx(long a, long b, long c) {
    return a * (b + bind_only(c));
}

/* 缺陷 1 的 file-scope 版本。 */
__attribute__((noinline)) static long bind_global(long q) {
    gv = q;
    sink[2] = gv;
    return gv;
}
__attribute__((noinline)) static long caller_holds_r14(long a, long b, long c, long d) {
    return a + (b * (c + bind_global(d)));
}

/* 反向对照：绑定必须仍然生效（值在 %rbx 里，而不是被悄悄退回内存）。 */
__attribute__((noinline)) static long pinned(long q) {
    register long v asm("rbx") = q;
    v = v + 1;
    return v;
}

int main(void) {
    long x1 = spill_clobbers_home(2, 3, 4);   /* (2+3)*5 + 7 = 32 */
    long x2 = caller_holds_rbx(10, 20, 30);   /* 10*(20+30)   = 500 */
    long x3 = caller_holds_r14(1, 2, 3, 40);  /* 1 + 2*(3+40) = 87 */
    long x4 = pinned(41);                     /* 42 */
    printf("x1=%ld x2=%ld x3=%ld x4=%ld v0=%ld v1=%ld v2=%ld\n", x1, x2, x3, x4,
           sink[0], sink[1], sink[2]);
    return (x1 == 32 && x2 == 500 && x3 == 87 && x4 == 42 && sink[0] == 7 &&
            sink[1] == 30 && sink[2] == 40) ? 0 : 1;
}
