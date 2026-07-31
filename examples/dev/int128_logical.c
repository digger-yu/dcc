/* __int128 上的 && / || 逻辑运算测试
 *
 * int128 的值用 rax:rdx 表示 (低 64 位在 rax, 高 64 位在 rdx).
 * __int128 视为真当且仅当 rax | rdx != 0, 即两半都为 0 时才为 0.
 * dcc 之前只 test rax (低 64 位), 高 1 低 0 的值会被误判为 0.
 */
typedef __int128 i128;
typedef unsigned __int128 u128;

/* 构造一个 "高 1 低 0" 的 __int128, 旧实现下会被判为 false. */
static i128 make_high_only(int high) {
    i128 v = (i128)high << 64;
    return v;
}

int test_int128_and_high_only(void) {
    /* (__int128)(1 << 64) && 1 == 1
     * 旧实现: test rax, rax = 0, jne 不跳, 走 l_false 分支返回 0.
     * 正确: 应当判为真, 评估 r (1), 结果为 1. */
    i128 a = make_high_only(1);
    int r = (a && 1);
    if (r != 1) return 1;
    return 0;
}

int test_int128_or_high_only(void) {
    /* (__int128)(1 << 64) || 0 == 1
     * 旧实现: test rax, rax = 0, jne 跳到 l_false 走 mov_imm(dst, 1),
     * 返回 1 (巧合正确); 但当左为 0 时旧实现也会返回 1, 不正确.
     * 正确行为: a 非 0, 短路求值, 返回 1. */
    i128 a = make_high_only(1);
    int r = (a || 0);
    if (r != 1) return 2;
    /* 真正的零值, a || 0 应当为 0 */
    i128 b = (i128)0;
    r = (b || 0);
    if (r != 0) return 3;
    return 0;
}

int test_int128_and_zero(void) {
    /* (__int128)0 && 1 == 0 */
    i128 a = (i128)0;
    int r = (a && 1);
    if (r != 0) return 4;
    return 0;
}

int test_int128_or_nonzero(void) {
    /* (__int128)5 || 0 == 1 */
    i128 a = (i128)5;
    int r = (a || 0);
    if (r != 1) return 5;
    return 0;
}

int test_int128_short_circuit(void) {
    /* 短路求值: 左为真时 && 不应评估右
     * 用一个除零副作用, 若评估了会触发 SIGFPE */
    i128 a = make_high_only(1);
    int r = (a && (1 / 1));
    if (r != 1) return 6;
    return 0;
}

int main(void) {
    int r = test_int128_and_high_only();
    if (r != 0) return 100 + r;
    r = test_int128_or_high_only();
    if (r != 0) return 200 + r;
    r = test_int128_and_zero();
    if (r != 0) return 300 + r;
    r = test_int128_or_nonzero();
    if (r != 0) return 400 + r;
    r = test_int128_short_circuit();
    if (r != 0) return 500 + r;
    return 0;
}
