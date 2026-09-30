/* Chained assignment: value of an assignment expression must be the
 * left operand converted to its type (C11 6.5.16p3), not the raw RHS.
 *
 * Root cause: the scalar Ident-assign path kept the untruncated 64-bit
 * RHS in the destination register, so the value propagated to the outer
 * assignment bypassed the narrowing conversion. store_stack always
 * writes 8 bytes, but slots are 8-byte aligned and reloaded by declared
 * width, so only the propagated register value was wrong:
 * ul = us = l2 = s2 = -1 gave ul == -1 instead of 65535.
 *
 * Fix: re-extend dst from the low 8/16 bits using the lvalue type's
 * signedness (movsx for signed incl. char, movzx for unsigned).
 *
 * GCC torture tests: 960801-1.c, 921218-2.c
 *
 * 不变式不止「寄存器里看着对」：赋值表达式的值必须是**存入之后**的对象值
 * (C11 6.5.16p3)，而每一层显式转换的宽度必须留在 IR 里。下面两组全局/局部
 * 链式赋值都只为这两条服务。
 */
void abort(void);
void exit(int);

unsigned f(void) {
    long long l2;
    unsigned short us;
    unsigned long long ul;
    short s2;
    ul = us = l2 = s2 = -1;
    return ul;
}

unsigned long long g(void) {
    long long l2;
    unsigned short us;
    unsigned long long ul;
    short s2;
    ul = us = l2 = s2 = -1;
    return ul;
}

short gs;
long gl;
unsigned short gu;

int main(void) {
    if (f() != (unsigned short) -1) abort();
    if (g() != (unsigned short) -1) abort();

    /* char is signed on x86-64: chain must propagate sign-extended */
    {
        char c;
        signed char sc;
        long l;
        l = c = sc = -1;
        if (c != -1 || sc != -1 || l != -1) abort();
    }

    /* unsigned narrow types truncate to the lvalue width */
    {
        unsigned char uc;
        unsigned int u;
        u = uc = 300;
        if (uc != 44 || u != 44) abort();
    }
    {
        unsigned short w;
        long m;
        m = w = 70000;
        if (w != 4464 || m != 4464) abort();
    }

    /* globals: 4/8-byte slots reload by declared width */
    gl = gu = gs = -1;
    if (gs != -1) abort();
    if (gu != (unsigned short) -1) abort();
    if (gl != (unsigned short) -1) abort();

    /* 中间一层的宽度不得被外层左值类型吃掉。两个独立缺陷都在这里：
     *   1. 构建器把预派发的 `target` 直接当 Cast 的 dest，于是
     *      `long w1 = (unsigned short)-1` 的 IR 里 `unsigned short` 那层消失，
     *      -O2 的 SCCP 将常量折成 -1（gcc 与 -O0 都是 65535）。
     *   2. 给全局量赋值后返回原始 RHS，违反 C11 6.5.16p3（赋值表达式的值是
     *      存入之后的对象值），链式赋值就漏掉了截断。*/
    {
        long w1;
        unsigned short uw;
        w1 = (unsigned short) -1;
        if (w1 != 65535) abort();
        long w2 = (unsigned char) 300;
        if (w2 != 44) abort();
        uw = 70000;
        if (uw != 4464) abort();
        long w3 = (short) uw;
        if (w3 != 4464) abort();
        int iv;
        long jv;
        jv = (iv = (unsigned short) -1);
        if (iv != 65535 || jv != 65535) abort();
    }

    exit(0);
}
