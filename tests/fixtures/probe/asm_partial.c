/* InlineAsm Partial: 一个输入 + 一个输出操作数, 通用 "r" 约束, %N 占位符需被替换 */
int asm_mid(int x) { int y; __asm__ __volatile__("mov %1, %0" : "=r"(y) : "r"(x)); return y; }
