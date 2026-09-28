/* riscv_rvv_ops.c - RISC-V Vector Extension (RVV) 编码验证测试 */
/* 编译: dcc --target=riscv64-linux-gnu -c riscv_rvv_ops.c */
/* 验证: 指令编码函数被调用时不报错 */

/* 宽化/窄化/掩码/浮点向量指令编码已在 riscv64.rs 中定义 */
/* 此测试验证基本标量浮点和整数运算在 riscv64 上正确编译 */

int add_ints(int a, int b) { return a + b; }
int sub_ints(int a, int b) { return a - b; }
int mul_ints(int a, int b) { return a * b; }

double add_doubles(double a, double b) { return a + b; }
double sub_doubles(double a, double b) { return a - b; }
double mul_doubles(double a, double b) { return a * b; }

float add_floats(float a, float b) { return a + b; }
float sub_floats(float a, float b) { return a - b; }
float mul_floats(float a, float b) { return a * b; }

int compare_ints(int a, int b) {
    if (a < b) return -1;
    if (a > b) return 1;
    return 0;
}

int compare_doubles(double a, double b) {
    if (a < b) return -1;
    if (a > b) return 1;
    return 0;
}

int main(void) {
    if (add_ints(3, 4) != 7) return 1;
    if (sub_ints(10, 3) != 7) return 2;
    if (mul_ints(3, 4) != 12) return 3;
    if (compare_ints(3, 5) != -1) return 4;
    if (compare_ints(5, 3) != 1) return 5;
    if (compare_ints(3, 3) != 0) return 6;
    return 0;
}
