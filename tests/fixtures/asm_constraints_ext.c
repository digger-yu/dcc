/* 内联 asm 约束扩展测试 (含 XMM "x" 约束) */

struct S { int a; int b; int c; };

int test_immediate_constraint(void) {
    int x = 0;
    asm volatile("mov $123, %0" : "=r"(x) : "i"(123));
    if (x != 123) return 1;
    return 0;
}

int test_address_constraint(void) {
    int x = 0;
    int *p = &x;
    asm volatile("mov $99, (%0)" : "=p"(p));
    if (x != 99) return 2;
    if (p != &x) return 3;
    return 0;
}

int test_general_constraint(void) {
    int x = 7;
    int y;
    asm volatile("mov $11, %0" : "=r"(y) : "g"(x));
    if (y != 11) return 4;
    return 0;
}

int test_constant_constraint(void) {
    int x = 0;
    asm volatile("mov %1, %0" : "=r"(x) : "X"(0x12345678));
    if (x != 0x12345678) return 5;
    return 0;
}

int test_match_constraint_0(void) {
    int a = 42;
    int b = 0;
    asm volatile("add $1, %0" : "=r"(b) : "0"(a));
    if (b != 43) return 6;
    return 0;
}

int test_early_clobber(void) {
    int in1 = 10, in2 = 20;
    int out = 0;
    asm volatile("mov %1, %0\n\tadd %2, %0"
                 : "=&r"(out) : "r"(in1), "r"(in2));
    if (out != 30) return 7;
    return 0;
}

int test_multi_constraint(void) {
    int x = 5;
    int y = 0;
    asm volatile("mov $8, %0" : "=rm"(y) : "rm"(x));
    if (y != 8) return 8;
    return 0;
}

int test_xmm_constraint(void) {
    float a = 1.0f;
    float b = 0.0f;
    asm volatile("movss %1, %0" : "=x"(b) : "x"(a));
    if (b != 1.0f) return 9;
    return 0;
}

int test_digit_label_constraint(void) {
    int x = 0;
    asm volatile goto(
        "mov $1, %0\n"
        "jmp %l[done]\n"
        "%=:\n"
        "mov $2, %0\n"
        "done:\n"
        "nop\n"
        : "=&r"(x) :: : done
    );
    if (x != 1) return 10;
    return 0;
}

int test_offsetof_constraint(void) {
    int x = 0;
    asm volatile("mov %1, %0" : "=r"(x) : "i"((int)__builtin_offsetof(struct S, b)));
    if (x != 4) return 11;
    return 0;
}

int main(void) {
    int r;
    r = test_immediate_constraint();     if (r) return r;
    r = test_address_constraint();       if (r) return r;
    r = test_general_constraint();       if (r) return r;
    r = test_constant_constraint();      if (r) return r;
    r = test_match_constraint_0();       if (r) return r;
    r = test_early_clobber();            if (r) return r;
    r = test_multi_constraint();         if (r) return r;
    r = test_xmm_constraint();           if (r) return r;
    r = test_digit_label_constraint();   if (r) return r;
    r = test_offsetof_constraint();      if (r) return r;
    return 0;
}
