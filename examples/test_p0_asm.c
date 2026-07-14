// 测试 P0 修复: inline asm 实际发射 + &&label

// 测试 1: 基本 inline asm 指令
int test_basic_asm() {
    __asm__ __volatile__("nop");
    __asm__ __volatile__("push %rax");
    __asm__ __volatile__("pop %rax");
    __asm__ __volatile__("mov %rax, %rbx");
    __asm__ __volatile__("add %rax, %rbx");
    __asm__ __volatile__("xor %rax, %rax");
    __asm__ __volatile__("mfence");
    __asm__ __volatile__("pause");
    return 0;
}

// 测试 2: 立即数操作
int test_asm_immediate() {
    __asm__ __volatile__("mov $1, %rax");
    __asm__ __volatile__("add $100, %rax");
    return 0;
}

// 测试 3: &&label (GNU 扩展)
int test_label_addr() {
    void *addr = &&my_label;
    if (addr == 0) return 5;
    goto *addr;
    return 6;
my_label:
    return 0;
}

int main() {
    int r;
    r = test_basic_asm();
    if (r) return 1;
    r = test_asm_immediate();
    if (r) return 2;
    r = test_label_addr();
    if (r) return 3;
    return 0;
}
