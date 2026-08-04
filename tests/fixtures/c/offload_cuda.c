// offload_cuda.c - CUDA/HIP 限定符 offload 元数据收集测试源
//   * `__global__` vec_add (第 7 行, 标识符位于 7:17) -> Cuda entry
//   * `__device__` square  (第 13 行, 标识符位于 13:16) -> Cuda entry
//   * `__host__`   host_only (第 17 行) -> 非 offload entry (host 侧)
//   * 只有 -fcuda / -fhip 时限定符才被接受; 否则报 E0009 等解析错误

__global__ void vec_add(int *a, int *b, int n) {
    for (int i = 0; i < n; i++) {
        a[i] = b[i] + n;
    }
}

__device__ int square(int x) {
    return x * x;
}

__host__ int host_only(int x) {
    return x + 1;
}
