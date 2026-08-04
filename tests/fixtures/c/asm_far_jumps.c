// Phase 2: 远跳转/远调用测试
// 编译: dcc -c -o /dev/null asm_far_jumps.c
// 预期: stderr 不包含 "emitted as nop"

// 1.12 far jumps: retf/lret/ljmp/lcall
void test_far_retf(void) {
    asm volatile("retf $8");
    asm volatile("lretq");
    asm volatile("lretw");
}

void test_far_ljmp(void) {
    asm volatile("ljmpl $0x08, $0x12345678");
    asm volatile("ljmpw $0x08, $0x1234");
}

void test_far_lcall(void) {
    asm volatile("lcalll $0x08, $0x12345678");
    asm volatile("lcallw $0x08, $0x1234");
}