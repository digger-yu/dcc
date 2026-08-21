/* m27_debugtrap_test.c - __builtin_debugtrap 汇编测试 */
/* 应发射 int3 指令 (0xcc) */
int main(void) {
    __builtin_debugtrap();
    return 0;
}