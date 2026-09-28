/* m27_unreachable_test.c - __builtin_unreachable 汇编测试 */
/* 应发射 ud2 指令 (0x0f 0x0b) */
int test_unreachable(int x) {
    if (x > 0) {
        return 1;
    }
    __builtin_unreachable();
    return 0;
}

int main(void) {
    return test_unreachable(1);
}