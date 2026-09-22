/* InlineAsm Minimal: 无操作数的 asm 语句 */
int asm_min(int x) { __asm__ __volatile__("nop"); return x; }
