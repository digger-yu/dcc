// offload_omp.c - OpenMP offload 元数据收集测试源
//   * `#pragma omp target` (第 10 行)   -> OmpTarget entry, 位置 10:1
//   * `#pragma omp target simd` (第 15 行) -> OmpTargetSimd entry, 位置 15:1
//   * `#pragma omp simd` (第 20 行)    -> OmpTargetSimd entry, 位置 20:1
//   * `#pragma omp parallel` (第 25 行) -> W0312 警告 (不支持构造, no-op)
//   * 函数外 `#pragma omp teams` (第 33 行) -> W0313 警告 (不支持构造, 忽略)
//   * 函数外 `#pragma omp target` (第 38 行) -> W0311 警告 (offload 构造在函数外, 忽略)

int offload_work(int x) {
#pragma omp target
    {
        x = x * 2;
    }

#pragma omp target simd
    for (int i = 0; i < 8; i++) {
        x += i;
    }

#pragma omp simd
    for (int i = 0; i < 4; i++) {
        x += i;
    }

#pragma omp parallel
    {
        x++;
    }

    return x;
}

#pragma omp teams
int plain_host(int y) {
    return y + 1;
}

#pragma omp target
