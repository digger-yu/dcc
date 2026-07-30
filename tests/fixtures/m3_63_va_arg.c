/* x86_64 SysV va_arg 完整支持 (Section 11.4)
 * __builtin_va_start / __builtin_va_arg / __builtin_va_end 由
 * x86_64 codegen 直接生成 va_list 读写指令 (gp_offset /
 * fp_offset / reg_save_area / overflow_arg_area). 无外部符号. */
typedef __builtin_va_list va_list;

/* 寄存器路径 (1-5 变参全部在 GP 寄存器) */
int sum_int5(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    int s = 0;
    for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, int);
    __builtin_va_end(ap);
    return s;
}

/* 溢出栈路径 (6+ 变参走 overflow_arg_area) */
int sum_int13(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    int s = 0;
    for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, int);
    __builtin_va_end(ap);
    return s;
}

/* long 类型 (8 字节步进) */
long sum_long5(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    long s = 0;
    for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, long);
    __builtin_va_end(ap);
    return s;
}

/* 浮点类型 (XMM reg + overflow 8 字节步进) */
double sum_double5(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    double s = 0.0;
    for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, double);
    __builtin_va_end(ap);
    return s;
}

/* 8 字节变参溢出栈 */
double sum_double8(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    double s = 0.0;
    for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, double);
    __builtin_va_end(ap);
    return s;
}
