/* m27_attr_ifunc.c - __attribute__((ifunc)) IFUNC 符号表验证 */
/* 验证: IFUNC 符号类型应为 STT_GNU_IFUNC, 解析器符号存在 */

/* IFUNC 解析器 */
static void *resolve_ifunc(void) {
    return (void *)default_impl;
}

/* IFUNC 实现 */
int default_impl(void) {
    return 1;
}

/* IFUNC 声明 */
int ifunc_func(void) __attribute__((ifunc("resolve_ifunc")));

int main(void) {
    int r = ifunc_func();
    if (r != 1 && r != 2) return 1;
    return 0;
}