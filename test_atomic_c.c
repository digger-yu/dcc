/* _Atomic C path 测试 - 使用 dcc 已识别的 _n 版本
 *
 * 关键说明: __atomic_compare_exchange_n 比较 *ptr 与 *expected,
 *   若相等则 *ptr = desired 并返回 1 (true); 否则 *expected = *ptr 并返回 0.
 *   本测试用 a=20, expected=5 触发"不匹配"路径, 验证返回值为 0.
 */
int g_counter = 0;

int main(void) {
    int a = 5;
    int expected = 5;

    /* fetch_add */
    int old = __atomic_fetch_add(&g_counter, 1, 5);  /* SEQ_CST */
    if (g_counter != 1) return 1;
    if (old != 0) return 2;

    /* load_n / store_n */
    __atomic_store_n(&a, 20, 5);
    int v = __atomic_load_n(&a, 5);
    if (v != 20) return 3;

    /* compare_exchange_n: a=20 vs expected=5, 不匹配 -> 返回 0 */
    int r = __atomic_compare_exchange_n(&a, &expected, 10, 0, 5, 5);
    if (r) return 4;  /* 应为 0, 若为 1 说明 CAS 错误成功 */
    /* CAS 失败时, expected 会被更新为 *ptr (= 20) */
    if (expected != 20) return 5;
    if (a != 20) return 6;  /* a 不变 */

    /* 现在重设 expected = 20, 再做 CAS 应该成功 */
    expected = 20;
    r = __atomic_compare_exchange_n(&a, &expected, 10, 0, 5, 5);
    if (!r) return 7;  /* 应为 1 */
    if (a != 10) return 8;
    if (expected != 20) return 9;  /* 成功时 expected 不变 */

    /* fetch_sub */
    int sub = __atomic_fetch_sub(&g_counter, 1, 5);
    if (g_counter != 0) return 10;
    if (sub != 1) return 11;

    /* fetch_xor, fetch_or, fetch_and */
    g_counter = 0xFF;
    int x = __atomic_fetch_xor(&g_counter, 0x0F, 5);
    if (g_counter != 0xF0) return 12;
    if (x != 0xFF) return 13;

    g_counter = 0xF0;
    x = __atomic_fetch_and(&g_counter, 0x0F, 5);
    if (g_counter != 0x00) return 14;
    if (x != 0xF0) return 15;

    g_counter = 0;
    x = __atomic_fetch_or(&g_counter, 0x55, 5);
    if (g_counter != 0x55) return 16;
    if (x != 0) return 17;

    /* _Atomic qualifier: 应该被解析为同类型 */
    int b = 0;
    _Atomic int *pa = (_Atomic int *)&b;  /* _Atomic 限定符在 ptr */
    __atomic_store_n(pa, 42, 5);
    if (b != 42) return 18;

    /* exchange_n */
    int ex = __atomic_exchange_n(&a, 99, 5);
    if (a != 99) return 19;
    if (ex != 10) return 20;

    return 0;
}
