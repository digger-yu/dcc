/* 测试 5 个运行时模块 (无外部依赖):
 * 1. 逗号运算符
 * 2. VLA 可变长数组
 * 3. 整数提升 (char/short)
 * 4. 内联汇编 __asm__
 * 返回 0 = 全部通过
 */

/* === 1. 逗号运算符 === */
int comma_test(void) {
    int i;
    int j;
    for (i = 0, j = 10; i < j; i++, j--) {
    }
    return i; /* 5 */
}

/* === 2. VLA 可变长数组 === */
int vla_test(int n) {
    int arr[n];
    int i;
    for (i = 0; i < n; i++) {
        arr[i] = i * i;
    }
    return arr[n - 1]; /* (n-1)^2 */
}

/* === 3. 整数提升 === */
int promotion_test(void) {
    char a = 200;
    char b = 100;
    int sum = a + b;
    /* signed char: 200 -> -56, -56 + 100 = 44 */
    return sum;
}

int short_test(void) {
    short x = 30000;
    short y = 10000;
    int sum = x + y;
    return sum > 30000 ? 1 : 0;
}

/* === 4. 内联汇编 __asm__ === */
void asm_test(void) {
    __asm__("nop");
    __asm__ volatile("nop");
    __asm__("mov %rax, %rax");
}

int main(void) {
    int fail = 0;

    /* 逗号运算符 */
    if (comma_test() != 5) fail = fail + 1;

    /* VLA */
    if (vla_test(5) != 16) fail = fail + 2;
    if (vla_test(8) != 49) fail = fail + 4;

    /* 整数提升 */
    if (promotion_test() != 44) fail = fail + 8;
    if (short_test() != 1) fail = fail + 16;

    /* 内联汇编 (不应崩溃) */
    asm_test();

    return fail; /* 0 = 全部通过 */
}
