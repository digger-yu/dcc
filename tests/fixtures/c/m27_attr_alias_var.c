/* m27_attr_alias_var.c - __attribute__((alias)) 变量别名测试 */
/* 验证: 变量 alias 符号应指向真实变量, 链接后可见且值相同 */

int real_var = 42;

/* alias_var 是 real_var 的别名 */
extern int alias_var __attribute__((alias("real_var")));

int main(void) {
    /* 别名变量应与真实变量有相同的值 */
    if (alias_var != 42) return 1;

    /* 修改别名应影响真实变量 (同一地址) */
    alias_var = 100;
    if (real_var != 100) return 2;

    return 0;
}
