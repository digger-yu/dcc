/* computed goto &&label 测试 (函数内) */
int test_within(int n) {
    void *ptrs[3];
    ptrs[0] = &&label_a;
    ptrs[1] = &&label_b;
    ptrs[2] = &&label_c;

    int i = n % 3;
    goto *ptrs[i];

label_a:
    return 1;
label_b:
    return 2;
label_c:
    return 3;
}

int main(void) {
    if (test_within(0) != 1) return 1;
    if (test_within(1) != 2) return 2;
    if (test_within(2) != 3) return 3;
    if (test_within(4) != 2) return 4;  /* 4 % 3 = 1 */
    if (test_within(9) != 1) return 5;  /* 9 % 3 = 0 */
    return 0;
}
