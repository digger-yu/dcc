// tests/fixtures/c/c11_atomic_macros.c
// C11 <stdatomic.h> 预置宏验证:
//   - hosted 模式: atomic_int / atomic_bool / memory_order_* 等应可用
//   - freestanding 模式: __STDC_NO_ATOMICS__=1, __STDC_HOSTED__=0

/* ---- 原子类型宏展开为等价非原子类型 ---- */
atomic_int   ai = 42;
atomic_bool  ab = 1;
atomic_long  al = 100L;
atomic_size_t asz = 0;

/* ---- 内存序常量 ---- */
int mo_relaxed = memory_order_relaxed;   /* 0 */
int mo_consume   = memory_order_consume; /* 1 */
int mo_acquire   = memory_order_acquire; /* 2 */
int mo_release   = memory_order_release; /* 3 */
int mo_acq_rel   = memory_order_acq_rel; /* 4 */
int mo_seq_cst   = memory_order_seq_cst; /* 5 */

/* ---- kill_dependency: 恒等宏 ---- */
int test_kill_dep(int x) {
    return kill_dependency(x);
}

/* ---- atomic_thread_fence / atomic_signal_fence ---- */
void test_fences(void) {
    atomic_thread_fence(memory_order_seq_cst);
    atomic_signal_fence(memory_order_acquire);
}

/* ---- __STDC_HOSTED__ 检查 ---- */
#if defined(__STDC_HOSTED__) && __STDC_HOSTED__ == 1
int hosted_ok = 1;
#endif

/* ---- C11 7.17.6: ATOMIC_*_LOCK_FREE 宏 ---- */
#if ATOMIC_BOOL_LOCK_FREE != 1
#error "ATOMIC_BOOL_LOCK_FREE should be 1"
#endif
#if ATOMIC_CHAR_LOCK_FREE != 1
#error "ATOMIC_CHAR_LOCK_FREE should be 1"
#endif
#if ATOMIC_SHORT_LOCK_FREE != 1
#error "ATOMIC_SHORT_LOCK_FREE should be 1"
#endif
#if ATOMIC_INT_LOCK_FREE != 1
#error "ATOMIC_INT_LOCK_FREE should be 1"
#endif
#if ATOMIC_LONG_LOCK_FREE != 1
#error "ATOMIC_LONG_LOCK_FREE should be 1"
#endif
#if ATOMIC_LLONG_LOCK_FREE != 1
#error "ATOMIC_LLONG_LOCK_FREE should be 1"
#endif
#if ATOMIC_POINTER_LOCK_FREE != 1
#error "ATOMIC_POINTER_LOCK_FREE should be 1"
#endif
int lock_free_ok = ATOMIC_INT_LOCK_FREE;

int main(void) {
    ai = 10;
    ab = 0;
    al = 200L;
    asz = 64;
    (void)test_kill_dep(7);
    test_fences();
    return (ai == 10 && ab == 0 && mo_seq_cst == 5) ? 0 : 1;
}
