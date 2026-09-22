/* SIMD 向量探针: 只用 vector_size 属性做整/浮加法, 不做下标 (Index 是独立能力) */
typedef int v4si __attribute__((vector_size(16)));
typedef float v4sf __attribute__((vector_size(16)));
v4si vadd(v4si a, v4si b) { return a + b; }
v4sf vfmul(v4sf a, v4sf b) { return a * b; }
