/* m27_attr_alias_invalid.c - alias 负向测试 */
/* 验证: alias 目标不存在应产生诊断 */

/* alias 指向不存在的符号 */
int alias_func(void) __attribute__((alias("nonexistent_func")));

int main(void) {
    return 0;
}