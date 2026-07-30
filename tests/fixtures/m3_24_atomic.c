int main(void) {
    int x = 0;
    // dcc 不预置 atomic_store / atomic_load 等 C11 stdatomic 宏 (会破坏
    // Linux 内核同名的 inline 函数)。用户需要 #include <stdatomic.h> 或
    // 直接使用 GCC __atomic_* 内建。
    __atomic_store_n(&x, 7, __ATOMIC_SEQ_CST);
    return __atomic_load_n(&x, __ATOMIC_SEQ_CST);
}
