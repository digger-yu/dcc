/* RISC-V D 扩展 + RVV 补全测试 */
/* 验证 double 精度浮点运算使用 D 扩展指令 */
/* 编译: dcc --target=riscv64-linux-gnu -c riscv_d_extension.c */

/* D 扩展: 双精度算术 */
double dadd(double a, double b) { return a + b; }
double dsub(double a, double b) { return a - b; }
double dmul(double a, double b) { return a * b; }
double ddiv(double a, double b) { return a / b; }

/* D 扩展: 双精度比较 */
int dlt(double a, double b) { return a < b; }
int dle(double a, double b) { return a <= b; }
int deq(double a, double b) { return a == b; }

/* D 扩展内建函数: sqrt/fmin/fmax */
extern double sqrt(double);
extern double fmin(double, double);
extern double fmax(double, double);

double dsqrt(double x) { return sqrt(x); }
double dfmin(double a, double b) { return fmin(a, b); }
double dfmax(double a, double b) { return fmax(a, b); }

int main(void) {
    double x = 2.0, y = 3.0;
    if (dadd(x, y) != 5.0) return 1;
    if (dsub(x, y) + 1.0 != 0.0) return 2;  /* avoid unary neg */
    if (dmul(x, y) != 6.0) return 3;
    if (!dlt(x, y)) return 4;
    if (!deq(dsqrt(4.0), 2.0)) return 5;
    if (!deq(dfmin(x, y), 2.0)) return 6;
    if (!deq(dfmax(x, y), 3.0)) return 7;
    return 0;
}
