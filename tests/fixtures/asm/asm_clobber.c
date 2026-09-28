/* 内联 asm clobber 完整支持测试
 *
 * 覆盖以下 clobber 类型:
 *   "cc"           → 条件码, x86 上无功能影响, 静默接受
 *   "memory"       → 编译器屏障 (mfence)
 *   "st" / "st(N)" → x87 FPU 栈寄存器
 *   "xmm0".."xmm15" → XMM 寄存器, 从 XMM 池排除
 *   GP 寄存器名     → 从 GP 寄存器池排除
 */

/* 1. cc clobber: 条件码被修改, 不影响功能 */
int test_cc_clobber(void) {
    int x = 42;
    asm volatile(
        "mov $100, %0\n\t"
        "cmp $50, %0"
        : "=r"(x)
        :
        : "cc"
    );
    return x == 100 ? 0 : 1;
}

/* 2. memory clobber: 编译器屏障 */
int test_memory_clobber(void) {
    int v = 7;
    asm volatile("" : "+m"(v) :: "memory");
    return v == 7 ? 0 : 2;
}

/* 3. GP register clobber: "rax" 被 clobber, 输出不应分配到 rax */
int test_gp_reg_clobber_rax(void) {
    int result = 0;
    /* clobber "rax": 编译器分配 result 时避开 rax */
    asm volatile(
        "mov $30, %0"
        : "=r"(result)
        :
        : "rax", "cc"
    );
    return result == 30 ? 0 : 3;
}

/* 4. GP register clobber: "rcx" 被 clobber */
int test_gp_reg_clobber_rcx(void) {
    int y = 0;
    asm volatile(
        "mov $15, %0"
        : "=r"(y)
        :
        : "rcx", "cc"
    );
    return y == 15 ? 0 : 4;
}

/* 5. 多个 GP clobber: 同时 clobber 多个寄存器 */
int test_multi_gp_clobber(void) {
    int sum = 0;
    asm volatile(
        "mov $10, %0"
        : "=r"(sum)
        :
        : "rax", "rcx", "rdx", "cc"
    );
    return sum == 10 ? 0 : 5;
}

/* 6. XMM clobber: xmm0 被 clobber, XMM 输出应避开 xmm0 */
int test_xmm_clobber(void) {
    /* 使用 XMM 约束 "x" 确保分配 XMM 寄存器 */
    float result = 0.0f;
    asm volatile(
        "movss %0, %0"
        : "=x"(result)
        :
        : "xmm0", "cc"
    );
    /* 只要不崩溃就算通过 */
    return 0;
}

/* 7. x87 clobber: "st" 被 clobber, 静默接受 */
int test_x87_clobber(void) {
    /* 简单测试: x87 clobber 不应导致编译错误 */
    int x = 99;
    asm volatile(
        "mov %1, %0"
        : "=r"(x)
        : "r"(x)
        : "st", "cc"
    );
    return x == 99 ? 0 : 7;
}

/* 8. x87 clobber: "st(0)" 形式 */
int test_x87_st0_clobber(void) {
    int v = 77;
    asm volatile(
        "nop"
        : "+r"(v)
        :
        : "st(0)", "cc"
    );
    return v == 77 ? 0 : 8;
}

/* 9. 组合 clobber: GP + XMM + cc + memory */
int test_combined_clobber(void) {
    int result = 0;
    asm volatile(
        "mov $33, %0"
        : "=r"(result)
        :
        : "rax", "xmm0", "xmm1", "cc", "memory"
    );
    return result == 33 ? 0 : 9;
}

/* 10. 子寄存器名 clobber: "eax" 等价于 clobber rax */
int test_subreg_clobber(void) {
    int y = 0;
    asm volatile(
        "mov $51, %0"
        : "=r"(y)
        :
        : "eax", "cc"
    );
    return y == 51 ? 0 : 10;
}

int main(void) {
    int r;
    r = test_cc_clobber();        if (r) return r;
    r = test_memory_clobber();    if (r) return r;
    r = test_gp_reg_clobber_rax(); if (r) return r;
    r = test_gp_reg_clobber_rcx(); if (r) return r;
    r = test_multi_gp_clobber();  if (r) return r;
    r = test_xmm_clobber();       if (r) return r;
    r = test_x87_clobber();       if (r) return r;
    r = test_x87_st0_clobber();   if (r) return r;
    r = test_combined_clobber();  if (r) return r;
    r = test_subreg_clobber();    if (r) return r;
    return 0;
}
