/* 原子探针: 内联 (lock/lr-sc/amo) 或 libcall 都算实现, 只要不报错且发出代码 */
int atomic_bump(int *p) { return __atomic_add_fetch(p, 1, 5); }
int atomic_cas(int *p, int e, int d) { return __atomic_compare_exchange_n(p, &e, d, 0, 5, 5); }
