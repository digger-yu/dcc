// 原子操作后端验证: fetch_add/sub/or/and/xor + load/store + exchange + 内存序常量
int main(void) {
    int x = 10;
    int old;

    // === 内存序常量验证 ===
    if (__ATOMIC_RELAXED != 0) return 1;
    if (__ATOMIC_ACQUIRE != 2) return 2;
    if (__ATOMIC_RELEASE != 3) return 3;
    if (__ATOMIC_SEQ_CST != 5) return 4;

    // === __atomic_fetch_add ===
    old = __atomic_fetch_add(&x, 5, __ATOMIC_SEQ_CST);
    if (old != 10) return 10;
    if (x != 15) return 11;

    // === __atomic_fetch_sub ===
    old = __atomic_fetch_sub(&x, 3, __ATOMIC_SEQ_CST);
    if (old != 15) return 12;
    if (x != 12) return 13;

    // === __atomic_load_n ===
    int v = __atomic_load_n(&x, __ATOMIC_SEQ_CST);
    if (v != 12) return 14;

    // === __atomic_store_n ===
    __atomic_store_n(&x, 42, __ATOMIC_SEQ_CST);
    if (x != 42) return 15;

    // === __atomic_fetch_or ===
    x = 0x0F;
    old = __atomic_fetch_or(&x, 0xF0, __ATOMIC_SEQ_CST);
    if (old != 0x0F) return 16;
    if (x != 0xFF) return 17;

    // === __atomic_fetch_and ===
    old = __atomic_fetch_and(&x, 0x0F, __ATOMIC_SEQ_CST);
    if (old != 0xFF) return 18;
    if (x != 0x0F) return 19;

    // === __atomic_fetch_xor ===
    x = 0xAA;
    old = __atomic_fetch_xor(&x, 0xFF, __ATOMIC_SEQ_CST);
    if (old != 0xAA) return 20;
    if (x != 0x55) return 21;

    // === __atomic_exchange_n ===
    x = 100;
    old = __atomic_exchange_n(&x, 200, __ATOMIC_SEQ_CST);
    if (old != 100) return 22;
    if (x != 200) return 23;

    return 0;
}
