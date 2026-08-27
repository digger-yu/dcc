// Test: asm goto with outputs/inputs for aarch64/riscv64
// Verifies that asm goto properly loads inputs and writes back outputs

int test_asm_goto_with_io(void) {
    int result = 0;
    int input = 42;
    __asm__ goto(
        "nop\n"           // aarch64: nop, riscv64: nop
        "cmp %1, #42\n"   // compare input with 42
        "beq %l[match]\n" // branch to match label if equal
        : "+r"(result)     // output: inout
        : "r"(input)       // input
        : "memory"
        : match
    );
    return result; // fallthrough: result = 0

match:
    result = 1;
    return result; // match: result = 1
}

int test_asm_basic_output(void) {
    int x = 10;
    int y = 0;
    __asm__(
        "add %0, %1, #5\n"  // y = x + 5
        : "=r"(y)
        : "r"(x)
    );
    return y; // 15
}

int main(void) {
    int r0 = test_asm_goto_with_io();
    int r1 = test_asm_basic_output();
    return r0 + r1; // 1 + 15 = 16
}
