/* InlineAsm Full: 多操作数 + clobber 列表 + %N 替换同时要成立 */
int asm_max(int x) {
    int y;
    __asm__ __volatile__("nop\n\tmov %1, %0" : "=r"(y) : "r"(x) : "memory", "cc");
    return y + x;
}
