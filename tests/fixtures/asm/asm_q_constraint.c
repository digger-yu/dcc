/* Test fixture: 'q' inline assembly constraint */

/* The 'q' constraint maps to GP registers: rax, rbx, rcx, rdx, rsi, rdi, rbp, r8-r15 */

int test_q_constraint_input(int a, int b)
{
    int result;
    __asm__ __volatile__(
        "addl %2, %0\n\t"
        : "=r"(result)
        : "0"(a), "q"(b)
    );
    return result;
}

int test_q_constraint_output(void)
{
    int result;
    __asm__ __volatile__(
        "movl $42, %0\n\t"
        : "=q"(result)
    );
    return result;
}

long test_q_constraint_64bit(long a, long b)
{
    long result;
    __asm__ __volatile__(
        "addq %2, %0\n\t"
        : "=r"(result)
        : "0"(a), "q"(b)
    );
    return result;
}

/* Test 'q' constraint with memory operand */
int test_q_constraint_with_memory(int *p)
{
    int result;
    __asm__ __volatile__(
        "movl (%1), %0\n\t"
        : "=q"(result)
        : "r"(p)
    );
    return result;
}
