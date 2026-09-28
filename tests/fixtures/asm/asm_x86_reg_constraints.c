/* Test fixture: x86-specific register constraints
 *
 * Covers: a (eax/rax), b (ebx/rbx), c (ecx/rcx), d (edx/rdx),
 *         S (esi/rsi), D (edi/rdi), q (byte regs), Q (broader byte),
 *         p (valid address for jump targets)
 *
 * These constraints are critical for kernel code like __cmpxchg
 * that relies on specific register assignments.
 */

/* 1. 'a' constraint — eax/rax */
int test_a_constraint_input(int val)
{
    int out;
    __asm__ volatile(
        "movl %1, %0\n\t"
        : "=r"(out)
        : "a"(val)
    );
    return out;
}

int test_a_constraint_output(void)
{
    int out;
    __asm__ volatile(
        "movl $0xAA, %0\n\t"
        : "=a"(out)
    );
    return out;
}

/* 2. 'b' constraint — ebx/rbx */
int test_b_constraint_input(int val)
{
    int out;
    __asm__ volatile(
        "movl %1, %0\n\t"
        : "=r"(out)
        : "b"(val)
    );
    return out;
}

int test_b_constraint_output(void)
{
    int out;
    __asm__ volatile(
        "movl $0xBB, %0\n\t"
        : "=b"(out)
    );
    return out;
}

/* 3. 'c' constraint — ecx/rcx */
int test_c_constraint_input(int val)
{
    int out;
    __asm__ volatile(
        "movl %1, %0\n\t"
        : "=r"(out)
        : "c"(val)
    );
    return out;
}

int test_c_constraint_output(void)
{
    int out;
    __asm__ volatile(
        "movl $0xCC, %0\n\t"
        : "=c"(out)
    );
    return out;
}

/* 4. 'd' constraint — edx/rdx */
int test_d_constraint_input(int val)
{
    int out;
    __asm__ volatile(
        "movl %1, %0\n\t"
        : "=r"(out)
        : "d"(val)
    );
    return out;
}

int test_d_constraint_output(void)
{
    int out;
    __asm__ volatile(
        "movl $0xDD, %0\n\t"
        : "=d"(out)
    );
    return out;
}

/* 5. 'S' constraint — esi/rsi */
int test_S_constraint_input(int val)
{
    int out;
    __asm__ volatile(
        "movl %1, %0\n\t"
        : "=r"(out)
        : "S"(val)
    );
    return out;
}

int test_S_constraint_output(void)
{
    int out;
    __asm__ volatile(
        "movl $0x55, %0\n\t"
        : "=S"(out)
    );
    return out;
}

/* 6. 'D' constraint — edi/rdi */
int test_D_constraint_input(int val)
{
    int out;
    __asm__ volatile(
        "movl %1, %0\n\t"
        : "=r"(out)
        : "D"(val)
    );
    return out;
}

int test_D_constraint_output(void)
{
    int out;
    __asm__ volatile(
        "movl $0xDD, %0\n\t"
        : "=D"(out)
    );
    return out;
}

/* 7. 'q' constraint — byte-addressable GP regs (a/b/c/d) */
int test_q_constraint_multi(int a, int b)
{
    int result;
    __asm__ volatile(
        "addl %2, %0\n\t"
        : "=r"(result)
        : "0"(a), "q"(b)
    );
    return result;
}

/* 8. 'Q' constraint — broader byte-addressable regs */
int test_Q_constraint_input(int val)
{
    int out;
    __asm__ volatile(
        "movl %1, %0\n\t"
        : "=r"(out)
        : "Q"(val)
    );
    return out;
}

/* 9. 'p' constraint — valid address (for jump targets) */
int test_p_constraint_addr(void)
{
    int val = 42;
    int out;
    __asm__ volatile(
        "movl (%1), %0\n\t"
        : "=r"(out)
        : "p"(&val)
    );
    return out;
}

/* 10. Combined: cmpxchg-like pattern with "=a" output and "a" input */
int test_cmpxchg_pattern(int old_val, int new_val, int *mem)
{
    int result;
    __asm__ volatile(
        "lock; cmpxchgl %2, %1\n\t"
        : "=a"(result), "+m"(*mem)
        : "r"(new_val), "a"(old_val)
        : "memory", "cc"
    );
    return result;
}

/* 11. Inout with fixed reg: "+a" */
int test_inout_a_constraint(int val)
{
    int io = val;
    __asm__ volatile(
        "addl $1, %0\n\t"
        : "+a"(io)
    );
    return io;
}

/* 12. Multiple fixed regs in one asm */
long test_multi_fixed_regs(long a, long b)
{
    long sum;
    __asm__ volatile(
        "addq %2, %0\n\t"
        : "=a"(sum)
        : "0"(a), "b"(b)
    );
    return sum;
}

int main(void)
{
    int r;

    r = test_a_constraint_input(0xAA);
    if (r != 0xAA) return 1;

    r = test_a_constraint_output();
    if (r != 0xAA) return 2;

    r = test_b_constraint_input(0xBB);
    if (r != 0xBB) return 3;

    r = test_b_constraint_output();
    if (r != 0xBB) return 4;

    r = test_c_constraint_input(0xCC);
    if (r != 0xCC) return 5;

    r = test_c_constraint_output();
    if (r != 0xCC) return 6;

    r = test_d_constraint_input(0xDD);
    if (r != 0xDD) return 7;

    r = test_d_constraint_output();
    if (r != 0xDD) return 8;

    r = test_S_constraint_input(0x55);
    if (r != 0x55) return 9;

    r = test_S_constraint_output();
    if (r != 0x55) return 10;

    r = test_D_constraint_input(0xDD);
    if (r != 0xDD) return 11;

    r = test_D_constraint_output();
    if (r != 0xDD) return 12;

    r = test_q_constraint_multi(10, 20);
    if (r != 30) return 13;

    r = test_Q_constraint_input(0x42);
    if (r != 0x42) return 14;

    r = test_p_constraint_addr();
    if (r != 42) return 15;

    int mem = 100;
    r = test_cmpxchg_pattern(100, 200, &mem);
    if (r != 100 || mem != 200) return 16;

    r = test_inout_a_constraint(99);
    if (r != 100) return 17;

    r = (int)test_multi_fixed_regs(1000, 2000);
    if (r != 3000) return 18;

    return 0;
}
