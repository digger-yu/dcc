// asm_x87_fpu.c — x87 FPU instructions test
// Tests all newly supported x87 FPU instructions

void test_x87_no_operand(void) {
    // x87 无操作数指令 (D9 编码空间)
    asm volatile("fnop");
    asm volatile("fchs");
    asm volatile("fabs");
    asm volatile("ftst");
    asm volatile("fxam");
    asm volatile("fld1");
    asm volatile("fldl2t");
    asm volatile("fldl2e");
    asm volatile("fldpi");
    asm volatile("fldlg2");
    asm volatile("fldln2");
    asm volatile("fldz");
    asm volatile("f2xm1");
    asm volatile("fyl2x");
    asm volatile("fptan");
    asm volatile("fpatan");
    asm volatile("fxtract");
    asm volatile("fprem1");
    asm volatile("fdecstp");
    asm volatile("fincstp");
    asm volatile("fprem");
    asm volatile("fyl2xp1");
    asm volatile("fsqrt");
    asm volatile("fsincos");
    asm volatile("frndint");
    asm volatile("fscale");
    asm volatile("fsin");
    asm volatile("fcos");
    asm volatile("fninit");
}

void test_x87_compare(void) {
    // x87 比较 (无操作数/栈顶)
    asm volatile("fcom");
    asm volatile("fcomp");
    asm volatile("fcompp");
    asm volatile("fucom");
    asm volatile("fucomp");
    asm volatile("fucomip");
}

void test_x87_mem_load_store(void) {
    double d;
    int i;

    // 浮点加载/存储
    asm volatile("fldl %0" : "=m"(d));
    asm volatile("fstl %0" : "=m"(d));
    asm volatile("fstpl %0" : "=m"(d));

    // 整数加载/存储
    asm volatile("fildl %0" : "=m"(i));
    asm volatile("fistl %0" : "=m"(i));
    asm volatile("fistpl %0" : "=m"(i));
}

void test_x87_control(void) {
    unsigned short cw;
    // fldcw/fnstcw/fnstsw
    asm volatile("fnstcw %0" : "=m"(cw));
    asm volatile("fldcw %0" : "=m"(cw));
    asm volatile("fnstsw %0" : "=m"(cw));
}

void test_x87_arith_mem(void) {
    double d;
    // 单精度内存算术
    asm volatile("fadds %0" : "=m"(d));
    asm volatile("fmuls %0" : "=m"(d));
    asm volatile("fsubs %0" : "=m"(d));
    asm volatile("fdivs %0" : "=m"(d));
}

void test_x87_bcd(void) {
    // BCD 加载/存储 (需要 80-bit 内存)
    // 仅测试编码, 不执行
    unsigned char buf[10];
    asm volatile("fbld %0" : "=m"(buf));
    asm volatile("fbstp %0" : "=m"(buf));
}

int main(void) {
    test_x87_no_operand();
    test_x87_compare();
    test_x87_mem_load_store();
    test_x87_control();
    test_x87_arith_mem();
    test_x87_bcd();
    return 0;
}