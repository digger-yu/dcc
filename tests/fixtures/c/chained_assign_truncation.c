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

    exit(0);
}
