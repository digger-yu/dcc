/* m27_attr_alias_weak.c - __weak + __attribute__((alias)) 内核模式测试 */
/* 验证: 内核中常见的 __weak + alias 符号覆盖模式 */

/* 弱符号默认实现 */
int __attribute__((weak)) __init_platform_early(void) {
    return 0;
}

/* 平台特定实现 */
int init_platform_early(void) {
    return 42;
}

/* alias 将弱符号指向平台实现 */
int __init_platform_early(void) __attribute__((alias("init_platform_early")));

/* 变量别名: 内核常见的符号导出模式 */
int platform_version = 1;
extern int plat_ver __attribute__((alias("platform_version")));

int main(void) {
    /* 函数别名应调用正确的实现 */
    if (__init_platform_early() != 42) return 1;

    /* 变量别名应访问同一值 */
    if (plat_ver != 1) return 2;

    return 0;
}
