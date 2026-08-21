/* m27_attr_alias.c - __attribute__((alias)) 符号别名测试 */
/* 验证: alias 符号应指向真实符号, 链接后可见 */

/* 真实函数 */
int real_func(void) {
    return 42;
}

/* alias: alias_func 是 real_func 的别名 */
int alias_func(void) __attribute__((alias("real_func")));

int main(void) {
    /* 函数别名应返回相同值 */
    if (alias_func() != 42) return 1;

    return 0;
}