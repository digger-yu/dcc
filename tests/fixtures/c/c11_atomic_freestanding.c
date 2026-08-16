// tests/fixtures/c/c11_atomic_freestanding.c
// freestanding 模式: __STDC_NO_ATOMICS__=1, __STDC_HOSTED__=0
// atomic_* 类型宏不应被预置

#if defined(__STDC_HOSTED__) && __STDC_HOSTED__ == 0
int hosted_is_0 = 1;
#endif

#if defined(__STDC_NO_ATOMICS__) && __STDC_NO_ATOMICS__ == 1
int no_atomics_is_1 = 1;
#endif

// C11 <stdatomic.h> 宏在 freestanding 模式必须全部不预置
// (与 Linux 内核同名内联函数/宏冲突)
#ifdef atomic_int
#error "atomic_int should not be preset in freestanding"
#endif
#ifdef atomic_char16_t
#error "atomic_char16_t should not be preset in freestanding"
#endif
#ifdef atomic_intmax_t
#error "atomic_intmax_t should not be preset in freestanding"
#endif
#ifdef memory_order_seq_cst
#error "memory_order_* should not be preset in freestanding"
#endif
#ifdef kill_dependency
#error "kill_dependency should not be preset in freestanding"
#endif
#ifdef atomic_thread_fence
#error "atomic_thread_fence should not be preset in freestanding"
#endif
#ifdef atomic_signal_fence
#error "atomic_signal_fence should not be preset in freestanding"
#endif
#ifdef ATOMIC_INT_LOCK_FREE
#error "ATOMIC_*_LOCK_FREE should not be preset in freestanding"
#endif

int main(void) {
    return (hosted_is_0 == 1 && no_atomics_is_1 == 1) ? 0 : 1;
}
