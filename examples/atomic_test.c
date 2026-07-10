/* 原子操作测试 */
int main(void) {
    int x = 10;
    int old;

    /* __atomic_fetch_add: 返回旧值, 加 val */
    old = __atomic_fetch_add(&x, 5, 0);
    if (old != 10) return 1;
    if (x != 15) return 2;

    /* __atomic_fetch_sub: 返回旧值, 减 val */
    old = __atomic_fetch_sub(&x, 3, 0);
    if (old != 15) return 3;
    if (x != 12) return 4;

    /* __atomic_load_n */
    int v = __atomic_load_n(&x, 0);
    if (v != 12) return 5;

    /* __atomic_store_n */
    __atomic_store_n(&x, 42, 0);
    if (x != 42) return 6;

    /* __atomic_fetch_or */
    x = 0x0F;
    old = __atomic_fetch_or(&x, 0xF0, 0);
    if (old != 0x0F) return 7;
    if (x != 0xFF) return 8;

    /* __atomic_fetch_and */
    old = __atomic_fetch_and(&x, 0x0F, 0);
    if (old != 0xFF) return 9;
    if (x != 0x0F) return 10;

    /* __atomic_fetch_xor */
    x = 0xAA;
    old = __atomic_fetch_xor(&x, 0xFF, 0);
    if (old != 0xAA) return 11;
    if (x != 0x55) return 12;

    return 0;
}
