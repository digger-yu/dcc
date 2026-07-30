/* Cast 形式 (__attribute__((vector_size(N))) type) x
 * 验证 dcc 解析 (__attribute__((__vector_size__ (N))) T)x 这种内联属性 cast.
 * 场景: libiberty/sha1.c 的 HAVE_X86_SHA1_HW_SUPPORT 路径
 *      及 GCC 内部头文件常见用法. */
typedef int int32x4_t_alias __attribute__((__vector_size__ (16)));

int scalar_to_via_attr(int x) {
    /* 内联属性 cast 形式: cast 表达式内部带 __attribute__ */
    int *p = (int *)((__attribute__((__vector_size__ (16))) int *)&x);
    return *p;
}

int main(void) {
    int x = 42;
    int *p = (int *)((__attribute__((__vector_size__ (16))) int *)&x);
    if (*p != 42) return 1;
    if (scalar_to_via_attr(x) != 42) return 2;
    return 0;
}
