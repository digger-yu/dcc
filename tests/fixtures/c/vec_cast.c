/* Cast 形式 (__attribute__((vector_size(N))) type) x
 * 验证 dcc 解析 (__attribute__((__vector_size__ (N))) T)x 这种内联属性 cast,
 * 并覆盖 vector/complex 同尺寸 cast 的 IR validator 与 x86_64 bitcast codegen.
 * 场景: libiberty/sha1.c 的 HAVE_X86_SHA1_HW_SUPPORT 路径
 *      及 GCC 内部头文件常见用法. */
typedef int int32x4_t_alias __attribute__((__vector_size__ (16)));
typedef int v2si __attribute__((vector_size(8)));
typedef unsigned int v2ui __attribute__((vector_size(8)));
typedef long long v1di __attribute__((vector_size(8)));

int scalar_to_via_attr(int x) {
    /* 内联属性 cast 形式: cast 表达式内部带 __attribute__ */
    int *p = (int *)((__attribute__((__vector_size__ (16))) int *)&x);
    return *p;
}

static int check_vector_to_scalar(void) {
    v2si v = {0x11223344, 0x55667788};
    unsigned long long bits = (unsigned long long)v;
    return bits == 0x5566778811223344ULL ? 0 : 10;
}

static int check_scalar_to_vector(void) {
    unsigned long long bits = 0x0102030405060708ULL;
    v2si v = (v2si)bits;
    int *p = (int *)&v;
    return p[0] == 0x05060708 && p[1] == 0x01020304 ? 0 : 20;
}

static int check_vector_to_vector(void) {
    v2ui u = {0x80000000U, 7U};
    v2si s = (v2si)u;
    v1di d = (v1di)s;
    unsigned int *p = (unsigned int *)&s;
    unsigned int *q = (unsigned int *)&d;
    return p[0] == 0x80000000U && p[1] == 7U &&
           q[0] == 0x80000000U && q[1] == 7U ? 0 : 30;
}

static int check_complex_to_vector(void) {
    float _Complex z = 1.0f + 2.0if;
    v2si v = (v2si)z;
    unsigned int *p = (unsigned int *)&v;
    return p[0] == 0x3f800000U && p[1] == 0x40000000U ? 0 : 40;
}

static int check_vector_to_complex(void) {
    v2si v = {0x40400000, 0x40800000};
    float _Complex z = (float _Complex)v;
    return __real__ z == 3.0f && __imag__ z == 4.0f ? 0 : 50;
}

int main(void) {
    int x = 42;
    int *p = (int *)((__attribute__((__vector_size__ (16))) int *)&x);
    if (*p != 42) return 1;
    if (scalar_to_via_attr(x) != 42) return 2;
    if (check_vector_to_scalar()) return 3;
    if (check_scalar_to_vector()) return 4;
    if (check_vector_to_vector()) return 5;
    if (check_complex_to_vector()) return 6;
    if (check_vector_to_complex()) return 7;
    return 0;
}
