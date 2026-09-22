/* long double 探针: 只用 80/128-bit 浮点运算, 不含 sizeof/offsetof */
long double lmultiply(long double a, long double b) { return a * b + 1.0L; }
long double lcmp(long double a) { return a > 0.5L ? a : -a; }
