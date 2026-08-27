/*
 * tests/fixtures/c/gcc_poison.c
 *
 * #pragma GCC poison 支持测试:
 * - poison 后的标识符使用时报错
 * - poison 前使用不报错
 * - 系统头文件中豁免
 */

/* 1. poison 前使用 printf 不报错 */
/* (此处不能实际调用 printf, 因为这是语法测试) */

/* 2. 标记 printf 和 fprintf 为禁用 */
#pragma GCC poison printf fprintf

/* 3. 正常代码不使用被 poison 的标识符 */
int poison_test_var = 42;

int main(void) {
    return poison_test_var == 42 ? 0 : 1;
}
