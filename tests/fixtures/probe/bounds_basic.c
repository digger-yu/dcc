/* 边界检查探针: 只有数组下标访问需要它 */
int bd_sum(int *a, int n) { int i, s = 0; for (i = 0; i < n; i++) s += a[i]; return s; }
