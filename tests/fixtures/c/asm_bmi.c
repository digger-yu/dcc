// asm_bmi.c — BMI1/BMI2 extensions test
// Tests all newly supported BMI1, BMI2, and related instructions

void test_bmi1(void) {
    unsigned long long a, b, c;

    // andn: dst = ~src1 & src2
    asm volatile("andn %1, %2, %0" : "=r"(c) : "r"(a), "r"(b));
    // bextr: dst = src1[src2.lo..src2.hi]
    asm volatile("bextr %1, %2, %0" : "=r"(c) : "r"(a), "r"(b));
    // blsi: dst = src & -src
    asm volatile("blsi %1, %0" : "=r"(c) : "r"(a));
    // blsmsk: dst = src ^ (src-1)
    asm volatile("blsmsk %1, %0" : "=r"(c) : "r"(a));
    // blsr: dst = src & (src-1)
    asm volatile("blsr %1, %0" : "=r"(c) : "r"(a));
    // bzhi: dst = src1[src2-1:0]
    asm volatile("bzhi %1, %2, %0" : "=r"(c) : "r"(a), "r"(b));
}

void test_bmi2(void) {
    unsigned long long a, b, c;

    // mulx: dst = src1 * src2 (no flags)
    asm volatile("mulx %1, %2, %0" : "=r"(c) : "r"(a), "r"(b));
    // pdep: dst = src1 with bits spread by src2 mask
    asm volatile("pdep %1, %2, %0" : "=r"(c) : "r"(a), "r"(b));
    // pext: dst = src1 with bits compressed by src2 mask
    asm volatile("pext %1, %2, %0" : "=r"(c) : "r"(a), "r"(b));
}

void test_bmi_shift(void) {
    unsigned long long a, b, c;

    // rorx: rotate right by imm8 (no flags)
    asm volatile("rorx $3, %1, %0" : "=r"(c) : "r"(a));
    asm volatile("rorx $8, %1, %0" : "=r"(c) : "r"(a));
    // sarx: dst = src1 >> src2 (arithmetic)
    asm volatile("sarx %1, %2, %0" : "=r"(c) : "r"(a), "r"(b));
    // shlx: dst = src1 << src2 (logical)
    asm volatile("shlx %1, %2, %0" : "=r"(c) : "r"(a), "r"(b));
    // shrx: dst = src1 >> src2 (logical)
    asm volatile("shrx %1, %2, %0" : "=r"(c) : "r"(a), "r"(b));
}

void test_bmi_adcx_adox(void) {
    unsigned long long a, b, c;

    // adcx: add with carry (CF only)
    asm volatile("adcx %1, %0" : "=r"(c) : "r"(a));
    // adox: add with overflow (OF only)
    asm volatile("adox %1, %0" : "=r"(c) : "r"(a));
}

int main(void) {
    test_bmi1();
    test_bmi2();
    test_bmi_shift();
    test_bmi_adcx_adox();
    return 0;
}