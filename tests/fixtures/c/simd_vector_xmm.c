/* SIMD 向量化测试: 验证 128-bit 向量使用 XMM 打包指令 */

typedef int v4si __attribute__((vector_size(16)));
typedef float v4sf __attribute__((vector_size(16)));
typedef double v2df __attribute__((vector_size(16)));

/* 整数向量加减 */
v4si int_add(v4si a, v4si b) { return a + b; }
v4si int_sub(v4si a, v4si b) { return a - b; }

/* 浮点向量算术 */
v4sf float_add(v4sf a, v4sf b) { return a + b; }
v4sf float_sub(v4sf a, v4sf b) { return a - b; }
v4sf float_mul(v4sf a, v4sf b) { return a * b; }
v4sf float_div(v4sf a, v4sf b) { return a / b; }

/* 双精度向量 */
v2df double_add(v2df a, v2df b) { return a + b; }
v2df double_mul(v2df a, v2df b) { return a * b; }

/* 位运算 */
v4si int_and(v4si a, v4si b) { return a & b; }
v4si int_or(v4si a, v4si b) { return a | b; }
v4si int_xor(v4si a, v4si b) { return a ^ b; }

int main(void) {
    v4si a = {1, 2, 3, 4};
    v4si b = {5, 6, 7, 8};
    v4si c = int_add(a, b);
    if (c[0] != 6 || c[1] != 8 || c[2] != 10 || c[3] != 12) return 1;

    v4sf fa = {1.0f, 2.0f, 3.0f, 4.0f};
    v4sf fb = {0.5f, 0.5f, 0.5f, 0.5f};
    v4sf fc = float_add(fa, fb);
    if (fc[0] != 1.5f) return 2;

    return 0;
}
