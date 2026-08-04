// asm_string_loop.c — String/loop/misc instructions test
// Tests all newly supported string, loop, and miscellaneous instructions

void test_string_no_operand(void) {
    // 字符串加载 (无操作数)
    asm volatile("lodsb");
    asm volatile("lodsw");
    asm volatile("lodsl");
    asm volatile("lodsd");
    asm volatile("lodsq");

    // 字符串比较 (无操作数)
    asm volatile("cmpsw");
    asm volatile("cmpsl");
    asm volatile("cmpsd");
    asm volatile("cmpsq");

    // 字符串扫描 (无操作数)
    asm volatile("scasw");
    asm volatile("scasl");
    asm volatile("scasd");
    asm volatile("scasq");
}

void test_loop_imm(void) {
    // 循环指令 (立即数偏移)
    asm volatile("loop $0");
    asm volatile("loop $127");
    asm volatile("loope $0");
    asm volatile("loopz $0");
    asm volatile("loopne $0");
    asm volatile("loopnz $0");
}

void test_misc(void) {
    // bound: 80286 legacy (发出 nop 警告)
    // enter: 栈帧创建
    asm volatile("enter $0, $0");
    asm volatile("enter $32, $0");
}

int main(void) {
    test_string_no_operand();
    test_loop_imm();
    test_misc();
    return 0;
}