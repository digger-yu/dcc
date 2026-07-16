/* 内联 asm 约束：=r / m / memory clobber */
int test_mov_output(void) {
    int x = 0;
    asm volatile("mov $42, %0" : "=r"(x));
    return x == 42 ? 0 : 1;
}

int test_memory_clobber(void) {
    int v = 7;
    asm volatile("" : "+m"(v) :: "memory");
    return v == 7 ? 0 : 2;
}

int main(void) {
    int r = test_mov_output();
    if (r) return r;
    r = test_memory_clobber();
    if (r) return r + 10;
    return 0;
}
