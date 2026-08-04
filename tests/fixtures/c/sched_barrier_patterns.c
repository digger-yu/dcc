// sched_barrier_patterns.c - kernel/sched 中常见的内存屏障模式
//
// 验证 dcc 能编译内核调度器中使用的各种 barrier / fence 内联汇编。
// 不执行（仅 -fsyntax-only / -c 编译测试）。

// smp_mb / smp_rmb / smp_wmb 的核心汇编
static inline void test_smp_mb(void) {
    __asm__ volatile("mfence" ::: "memory");
}

static inline void test_smp_rmb(void) {
    __asm__ volatile("lfence" ::: "memory");
}

static inline void test_smp_wmb(void) {
    __asm__ volatile("sfence" ::: "memory");
}

// barrier() — 编译器屏障
static inline void test_barrier(void) {
    __asm__ volatile("" ::: "memory");
}

// pause 指令 (busy-wait 循环中使用)
static inline void test_cpu_relax(void) {
    __asm__ volatile("rep; nop" ::: "memory");
}

// LOCK 前缀的原子操作 (sched 中频繁使用)
static inline void test_lock_prefix(void) {
    int val = 0;
    __asm__ volatile(
        "lock; addl $1, %0"
        : "+m"(val)
        :
        : "memory"
    );
}

// xchg (隐含 lock)
static inline int test_xchg(int *ptr, int new_val) {
    int old;
    __asm__ volatile(
        "xchgl %0, %1"
        : "=r"(old), "+m"(*ptr)
        : "0"(new_val)
        : "memory"
    );
    return old;
}

// cmpxchg (sched 中 try_to_wake_up 等路径使用)
static inline int test_cmpxchg(int *ptr, int old_val, int new_val) {
    int prev;
    __asm__ volatile(
        "lock; cmpxchgl %2, %1"
        : "=a"(prev), "+m"(*ptr)
        : "r"(new_val), "0"(old_val)
        : "memory", "cc"
    );
    return prev;
}

void sched_barrier_test_entry(void) {
    test_smp_mb();
    test_smp_rmb();
    test_smp_wmb();
    test_barrier();
    test_cpu_relax();
    test_lock_prefix();
    int v = 0;
    test_xchg(&v, 1);
    test_cmpxchg(&v, 1, 2);
}
