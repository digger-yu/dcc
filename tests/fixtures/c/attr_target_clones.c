// Test __attribute__((target_clones(...)))
// 生成多版本函数和 IFUNC resolver

#include <stdio.h>

// 基础版本: SSE4.2 和 default
__attribute__((target_clones("sse4.2,default")))
int compute(int x) {
    return x * 2 + 1;
}

// 多版本: AVX, SSE4.2, default
__attribute__((target_clones("avx,sse4.2,default")))
int multiply(int a, int b) {
    return a * b;
}

int main() {
    int r1 = compute(10);
    int r2 = multiply(3, 4);
    
    printf("compute(10) = %d\n", r1);
    printf("multiply(3, 4) = %d\n", r2);
    
    // 验证变体符号存在
    // compute.sse4.2, compute.default
    // multiply.avx, multiply.sse4.2, multiply.default
    
    return (r1 == 21 && r2 == 12) ? 0 : 1;
}
