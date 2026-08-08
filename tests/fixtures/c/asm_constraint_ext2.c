/* 约束系统扩展测试: 段寄存器/x87/MMX/条件码/显式子寄存器约束 */

/* 1. 显式子寄存器约束 {eax}/{cx}/{cl} */
int test_explicit_reg_eax(void) {
    int x = 0;
    asm volatile("movl $42, %0" : "={eax}"(x));
    if (x != 42) return 1;
    return 0;
}

int test_explicit_reg_ecx(void) {
    int x = 0;
    asm volatile("movl $99, %0" : "={ecx}"(x));
    if (x != 99) return 2;
    return 0;
}

int test_explicit_reg_cl(void) {
    unsigned char x = 0;
    asm volatile("movb $7, %0" : "={cl}"(x));
    if (x != 7) return 3;
    return 0;
}

int test_explicit_reg_cx(void) {
    unsigned short x = 0;
    asm volatile("movw $0xBEEF, %0" : "={cx}"(x));
    if (x != 0xBEEF) return 4;
    return 0;
}

int test_explicit_reg_input(void) {
    int val = 0x1234;
    int result = 0;
    asm volatile("movl %1, %0" : "={eax}"(result) : "{edx}"(val));
    if (result != val) return 5;
    return 0;
}

/* 2. GCC 条件码约束 @ccz/@ccs/@ccc/@ccnz/@ccne */
int test_cc_z_flag_zero(void) {
    int flag = -1;
    int a = 0;
    asm("testl %1, %1" : "=@ccz"(flag) : "r"(a));
    /* 0 & 0 = 0 → ZF=1, flag should be 1 */
    if (flag != 1) return 6;
    return 0;
}

int test_cc_z_flag_nonzero(void) {
    int flag = -1;
    int a = 5;
    asm("testl %1, %1" : "=@ccz"(flag) : "r"(a));
    /* 5 & 5 = 5 ≠ 0 → ZF=0, flag should be 0 */
    if (flag != 0) return 7;
    return 0;
}

int test_cc_s_flag_negative(void) {
    int flag = -1;
    int a = -1;
    asm("testl %1, %1" : "=@ccs"(flag) : "r"(a));
    /* -1 & -1 = -1, SF=1, flag should be 1 */
    if (flag != 1) return 8;
    return 0;
}

int test_cc_s_flag_positive(void) {
    int flag = -1;
    int a = 1;
    asm("testl %1, %1" : "=@ccs"(flag) : "r"(a));
    /* 1 & 1 = 1, SF=0, flag should be 0 */
    if (flag != 0) return 9;
    return 0;
}

int test_cc_nz_flag(void) {
    int flag = -1;
    int a = 0;
    asm("testl %1, %1" : "=@ccnz"(flag) : "r"(a));
    /* 0 & 0 = 0 → ZF=1 → nz=false → flag=0 */
    if (flag != 0) return 10;
    return 0;
}

int test_cc_c_flag(void) {
    int flag = -1;
    unsigned int a = 0xFFFFFFFFu;
    unsigned int b = 1;
    asm("addl %2, %1" : "=@ccc"(flag), "+r"(a) : "r"(b));
    /* 0xFFFFFFFF + 1 = overflow → CF=1, flag should be 1 */
    if (flag != 1) return 11;
    return 0;
}

/* 3. x87 FPU 约束 f/t/u */
int test_x87_output_t(void) {
    double val = 0.0;
    asm volatile("fld1" : "=t"(val));
    /* fld1 pushes 1.0; writeback fstp stores 1.0 to val */
    if (val != 1.0) return 12;
    return 0;
}

int test_x87_readwrite_t(void) {
    double val = 4.0;
    asm volatile("fsqrt" : "+t"(val));
    /* sqrt(4.0) = 2.0 */
    if (val < 1.99 || val > 2.01) return 13;
    return 0;
}

int test_x87_f_constraint(void) {
    double val = 0.0;
    asm volatile("fld1" : "=f"(val));
    if (val != 1.0) return 14;
    return 0;
}

int test_x87_fldz(void) {
    double val = 99.0;
    asm volatile("fldz" : "=t"(val));
    /* fldz pushes 0.0 */
    if (val != 0.0) return 15;
    return 0;
}

/* 4. MMX 寄存器约束 y */
int test_mmx_constraint(void) {
    long long x = 0;
    long long y = 0x123456789ABCDEF0LL;
    asm volatile("movq %1, %0" : "=y"(x) : "y"(y));
    if (x != y) return 16;
    return 0;
}

int test_mmx_constraint2(void) {
    long long x = 0;
    long long a = 0x1111111111111111LL;
    long long b = 0x2222222222222222LL;
    asm volatile("movq %1, %0\n\t"
                 "por %2, %0"
                 : "=&y"(x) : "y"(a), "y"(b));
    if (x != (a | b)) return 17;
    return 0;
}

/* 5. 段寄存器约束 {gs}/{fs} (只读, 不修改段寄存器) */
int test_seg_register_gs(void) {
    unsigned short gs_val = 0;
    /* 空模板: 只捕获 %gs 的当前值到变量 */
    asm volatile("" : "={gs}"(gs_val));
    /* 用户态 Linux x86-64 下 %gs 选择子可能为 0 (基址在 MSR_GS_BASE),
       只验证不崩溃即可 */
    (void)gs_val;
    return 0;
}

int test_seg_register_fs(void) {
    unsigned short fs_val = 0;
    asm volatile("" : "={fs}"(fs_val));
    (void)fs_val;
    return 0;
}

int main(void) {
    int r;
    /* 显式子寄存器 */
    r = test_explicit_reg_eax();      if (r) return r;
    r = test_explicit_reg_ecx();      if (r) return r;
    r = test_explicit_reg_cl();       if (r) return r;
    r = test_explicit_reg_cx();       if (r) return r;
    r = test_explicit_reg_input();    if (r) return r;
    /* 条件码 */
    r = test_cc_z_flag_zero();        if (r) return r;
    r = test_cc_z_flag_nonzero();     if (r) return r;
    r = test_cc_s_flag_negative();    if (r) return r;
    r = test_cc_s_flag_positive();    if (r) return r;
    r = test_cc_nz_flag();            if (r) return r;
    r = test_cc_c_flag();             if (r) return r;
    /* x87 */
    r = test_x87_output_t();          if (r) return r;
    r = test_x87_readwrite_t();      if (r) return r;
    r = test_x87_f_constraint();      if (r) return r;
    r = test_x87_fldz();             if (r) return r;
    /* MMX */
    r = test_mmx_constraint();        if (r) return r;
    r = test_mmx_constraint2();       if (r) return r;
    /* 段寄存器 */
    r = test_seg_register_gs();       if (r) return r;
    r = test_seg_register_fs();       if (r) return r;
    return 0;
}
