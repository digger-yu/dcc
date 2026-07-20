/* 浮点取模 (fmod) 测试
 *
 * x86 没有原生 float 模指令, dcc 走 fmodf / fmod C 库.
 * 验证基本算术正确性以及与 GCC fmod 语义一致.
 */
#include <math.h>

int test_float_mod_basic(void) {
    float a = 7.0f, b = 3.0f;
    float r = a % b;
    /* fmod(7, 3) = 1 */
    if (r != 1.0f) return 1;
    return 0;
}

int test_double_mod_basic(void) {
    double a = 10.5, b = 3.0;
    double r = a % b;
    /* fmod(10.5, 3.0) = 1.5 */
    if (r != 1.5) return 2;
    return 0;
}

int test_float_mod_negative(void) {
    /* fmod 保留被除数符号: fmod(-7, 3) = -1 */
    float a = -7.0f, b = 3.0f;
    float r = a % b;
    if (r != -1.0f) return 3;
    return 0;
}

int test_float_mod_exact(void) {
    /* fmod(6.0, 3.0) = 0 */
    float a = 6.0f, b = 3.0f;
    float r = a % b;
    if (r != 0.0f) return 4;
    return 0;
}

int test_double_mod_small(void) {
    /* fmod(1.5, 0.5) = 0 */
    double a = 1.5, b = 0.5;
    double r = a % b;
    if (r != 0.0) return 5;
    return 0;
}

int test_double_mod_expr(void) {
    /* 嵌套表达式 */
    double r = 10.5 % 3.0;
    if (r != 1.5) return 6;
    return 0;
}

int main(void) {
    int r = test_float_mod_basic();
    if (r != 0) return 100 + r;
    r = test_double_mod_basic();
    if (r != 0) return 200 + r;
    r = test_float_mod_negative();
    if (r != 0) return 300 + r;
    r = test_float_mod_exact();
    if (r != 0) return 400 + r;
    r = test_double_mod_small();
    if (r != 0) return 500 + r;
    r = test_double_mod_expr();
    if (r != 0) return 600 + r;
    return 0;
}
