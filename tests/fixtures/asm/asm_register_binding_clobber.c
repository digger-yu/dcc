/* `register asm` 绑定的寄存器是向调用者「借」的：借之前必须保存，借期间别人
 * 不能再把它当临时值或操作数发出去。
 *
 * 四条实测缺陷里第 1~3 条出现在 -O0/-O1，而 -O2/-O3 恰好躲过 —— SSA 往返换了表达式
 * 形状，没走到那条溅出路径。第 4 条与表达式形状无关，四个档位都坏。所以这份夹具
 * 必须逐档与 gcc 对照：只看默认档等于没测，只看 -O2 会以为它一直是对的。
 *
 *  1. 被调方绑了 %rbx 却不保存它。序言里那四条 callee-saved 保存是先无条件
 *     发射、函数体生成完后由 `patch_callee_saves` 把「本函数没用到」的改成 nop;
 *     绑定路径从没登记过 `callee_saved_used`，于是保存被改成 nop，调用方跨调用
 *     存在 %rbx 里的值就此消失。修前实测（`caller_holds_rbx(10,20,30)`）：
 *         dcc -O0: r=1500     gcc: r=500
 *     file-scope 的绑定是另一条路：`compile_func` 开头就把绑定写进表，而位图在
 *     其后被 `reset_reg_tracker()` 清零，所以必须在那里重新登记（这一条造不出
 *     稳定的运行时错值 —— 临时值池归还得很即时 —— 由测试里的产物字节断言钉）。
 *  2. 同一函数里的表达式溅出池不知道 %rbx 已经是某个变量的家，把它当临时值发了
 *     出去，直接盖掉变量的家。修前实测（`v` 应为 7）：
 *         dcc -O0: x1=50 v0=25    gcc: x1=32 v0=7
 *  3. 内联汇编的 `q` 约束也不认识绑定表：它的 legacy 池是 [rax,rbx,rcx,rdx]，
 *     既不看变量绑定也不看表达式溅出槽。四个 `"=q"` 输出实测把值写进了绑在 %rbx
 *     的变量里（`q_pool_vs_binding`：修后 v 仍是 100，修前是 22）。
 *  4. 规划器 `pick` 的第二遍「兜底」忽略 `used` 集合，把**已经发出去**的号再发
 *     一次。六个 `"=q"` 输出因此互相重叠：修前实测 `six_q_outputs()` 得
 *     `six=297 o0=66`，修后与 gcc 一致：`six=231 o0=11`。这一条四个档位都坏
 *     （它与表达式形状无关，不像第 1~3 条只在 -O0/-O1 暴露）。
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

/* 缺陷 3：`q` 类的 legacy 池不认识绑定表 —— 四个输出实测盖掉绑在 %rbx 的 v。 */
__attribute__((noinline)) static long q_pool_vs_binding(long *out_sum) {
    register long v asm("rbx") = 100;
    long o1 = 0, o2 = 0, o3 = 0, o4 = 0;
    __asm__ volatile("movq $11, %0\n\tmovq $22, %1\n\tmovq $33, %2\n\tmovq $44, %3"
                     : "=q"(o1), "=q"(o2), "=q"(o3), "=q"(o4));
    *out_sum = o1 + o2 + o3 + o4;   /* 110 */
    return v;                       /* 必须还是 100 */
}

/* 缺陷 4：规划器 `pick` 的第二遍「兜底」忽略 `used`，把已经发出去的号再发一次 ——
 * 六个 `"=q"` 输出因此互相重叠：修前 `six_q_outputs()` 得 `six=297 o0=66`（第一个输出
 * 被最后一个盖掉），修后与 gcc 一致：`six=231 o0=11`。
 * 操作数用六个独立标量而不是数组元素：「寄存器类输出绑到非 Ident 左值」另有独立
 * 缺陷（写回整个丢掉），由 tests/fixtures/c/asm_output_general_lvalue.c 单独钉。
 */
__attribute__((noinline)) static long six_q_outputs(long *o0) {
    long a = 0, b = 0, c = 0, d = 0, e = 0, f = 0;
    __asm__ volatile(
        "movq $11, %0\n\tmovq $22, %1\n\tmovq $33, %2\n\t"
        "movq $44, %3\n\tmovq $55, %4\n\tmovq $66, %5"
        : "=q"(a), "=q"(b), "=q"(c), "=q"(d), "=q"(e), "=q"(f));
    *o0 = a;
    return a + b + c + d + e + f;            /* 11+22+33+44+55+66 = 231 */
}

/* 缺陷 5：变量绑在寄存器里时，内联汇编的输出写回必须落到那个寄存器 —— 旧代码无条件
 * 写栈槽，于是结果落进一个再没人读的内存里。修前实测
 *   register long v asm("rbx"); __asm__("movq $5, %0" : "=r"(v)); → v = 0（gcc 是 5）*/
__attribute__((noinline)) static long asm_output_into_bound(void) {
    register long v asm("rbx") = 0;
    __asm__ volatile("movq $5, %0" : "=r"(v));
    v += 1;
    return v;                              /* 6 */
}

/* 反向对照：绑定必须仍然生效（值在 %rbx 里，而不是被悄悄退回内存）。 */
__attribute__((noinline)) static long pinned(long q) {
    register long v asm("rbx") = q;
    v = v + 1;
    return v;
}

int main(void) {
    long qsum = 0, o0 = 0;
    long x1 = spill_clobbers_home(2, 3, 4);   /* (2+3)*5 + 7 = 32 */
    long x2 = caller_holds_rbx(10, 20, 30);   /* 10*(20+30)   = 500 */
    long x3 = caller_holds_r14(1, 2, 3, 40);  /* 1 + 2*(3+40) = 87 */
    long x4 = pinned(41);                     /* 42 */
    long xv = q_pool_vs_binding(&qsum);       /* v=100, qsum=110 */
    long x6 = six_q_outputs(&o0);             /* 231, o0=11 */
    long x5 = asm_output_into_bound();          /* 5 + 1 = 6 */
    printf("x1=%ld x2=%ld x3=%ld x4=%ld x5=%ld v=%ld qsum=%ld six=%ld o0=%ld sink0=%ld\n",
           x1, x2, x3, x4, x5, xv, qsum, x6, o0, sink[0]);
    return (x1 == 32 && x2 == 500 && x3 == 87 && x4 == 42 && x5 == 6 && xv == 100 && qsum == 110 &&
            x6 == 231 && o0 == 11 && sink[0] == 7) ? 0 : 1;
}
