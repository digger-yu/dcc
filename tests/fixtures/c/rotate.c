/* builtin_rotate_clrsb.c - 验证 rotate / clrsb / fma 内建 */
/* 提供本地 fma 实现以支持 builtin 调用. */

int test_rotate(void) {
    unsigned int r1 = __builtin_rotateleft32(0x80000001u, 1);   /* 0x00000003 */
    unsigned int r2 = __builtin_rotateright32(0x00000003u, 1);  /* 0x80000001 */
    unsigned long long rl1 = __builtin_rotateleft64(0x8000000000000001ull, 1);
    unsigned short rs1 = __builtin_rotateleft16(0x8001, 1);     /* 0x0003 */
    unsigned char rc1 = __builtin_rotateright8(0xC1, 2);        /* 0x70 */
    unsigned long long rl2 = __builtin_rotateright64(rl1, 1);   /* 0x8000000000000001 */
    if (r1 != 0x00000003u) return 1;
    if (r2 != 0x80000001u) return 2;
    if (rs1 != 0x0003u) return 3;
    if (rc1 != 0x70u) return 4;
    if (rl1 != 0x0000000000000003LL) return 5;
    if (rl2 != 0x8000000000000001LL) return 6;
    return 0;
}

int test_clrsb(void) {
    /* __builtin_clrsb: 符号位冗余的位数 = clz(x ^ (x>>31)) - 1 */
    int c1 = __builtin_clrsb(0x00000001);   /* 30 (前导 30 个 0) */
    int c2 = __builtin_clrsb(0x80000000);   /* 0  (仅符号位) */
    int c3 = __builtin_clrsb(0xFFFFFFFF);   /* 31 (全符号位) */
    int c4 = __builtin_clrsb(0x00000000);   /* 31 (0 视作正数) */
    long long cll1 = __builtin_clrsbll(0x100000000LL);  /* 30 */
    int cl1 = __builtin_clrsbl(0x80000000);             /* 0  (32 位) */
    if (c1 != 30) return 11;
    if (c2 != 0) return 12;
    if (c3 != 31) return 13;
    if (c4 != 31) return 14;
    if (cll1 != 30) return 15;
    if (cl1 != 0) return 16;
    return 0;
}

/* 本地 fma/fmaf 实现, codegen 走 libc 路径时会调用这里 */
double fma(double a, double b, double c) {
    return a * b + c;
}
float fmaf(float a, float b, float c) {
    return a * b + c;
}

int test_fma(void) {
    /* fma(1.0, 2.0, 3.0) = 5.0 */
    double r = __builtin_fma(1.0, 2.0, 3.0);
    float rf = __builtin_fmaf(1.0f, 2.0f, 3.0f);
    if (r != 5.0) return 21;
    if (rf != 5.0f) return 22;
    return 0;
}

int main(void) {
    int r;
    if ((r = test_rotate()) != 0) return r;
    if ((r = test_clrsb()) != 0) return r;
    if ((r = test_fma()) != 0) return r;
    return 0;
}
