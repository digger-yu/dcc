/* m27_trap_test.c - __builtin_trap 汇编测试 */
/* 应发射 ud2 指令 (0x0f 0x0b) */
int main(void) {
    __builtin_trap();
    return 0;
}