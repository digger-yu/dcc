/* 已退化为指针的 va_list 上取 __int128：分支缺失 + 边界判据两处缺陷。
 *
 * 覆盖缺陷 D6：`gen_va_arg_ptr` 曾完全没有 `__int128` 分支，16 字节的标量落回
 * 单 8 字节通用路径 —— 只取到（也只推进）低半。与 D5 同源的判据现由
 * `gen_va_arg_int128_at` 在 named / global / ptr 三种落点上共用。
 *
 * 本用例走两个极端，各自可独立归因：
 *   - `take_regs`：游标在 8（两半都在寄存器保存区）→ 验证 rdx:rax 两半都取到；
 *   - `take_mem` ：游标被推到 40（装不进 → 整体上溢出区且不推进 gp_offset）
 *     → 验证不拆分、且后续参数位置不错位。
 *
 * `va_list` 形参在原型上已退化为指针，所以两条路径都走 `[rcx+...]` 寻址，
 * 与 `helper` 自己的寄存器保存区之间靠指针传递衔接。
 */
typedef __builtin_va_list va_list;

static __int128 mk(long long lo, long long hi) {
    return ((__int128) hi << 64) | (unsigned long long) lo;
}
static long long lo_of(__int128 v) {
    return (long long) v;
}
static long long hi_of(__int128 v) {
    return (long long)(v >> 64);
}

static int take_regs(int n, va_list ap) {
    (void) n;
    __int128 a = __builtin_va_arg(ap, __int128);
    long long b = __builtin_va_arg(ap, long long);
    int r = 0;
    if (lo_of(a) != 0x1122334455667788LL)
        r |= 1;
    if (hi_of(a) != 0x0fedcba987654321LL)
        r |= 2;
    if (b != 44)
        r |= 4;
    return r;
}

static int take_mem(int n, va_list ap) {
    (void) n;
    for (int i = 0; i < 4; i++) /* gp_offset: 8 -> 40 */
        (void) __builtin_va_arg(ap, long long);
    __int128 a = __builtin_va_arg(ap, __int128);
    long long b = __builtin_va_arg(ap, long long);
    int r = 0;
    if (lo_of(a) != 31 || hi_of(a) != 32)
        r |= 8;
    if (b != 55)
        r |= 16;
    return r;
}

/* 两个入口各自只跑一条路径，避免为另一条路径准备参数时越界读栈。
 * 位段划分：take_regs 用 bits 1|2|4，take_mem 用 bits 8|16。 */
static int helper_regs(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    int r = take_regs(n, ap);
    __builtin_va_end(ap);
    return r;
}

static int helper_mem(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    int r = take_mem(n, ap);
    __builtin_va_end(ap);
    return r;
}

int main(void) {
    int r = helper_regs(1, mk(0x1122334455667788LL, 0x0fedcba987654321LL), 44LL);
    r |= helper_mem(1, 10LL, 20LL, 30LL, 40LL, mk(31, 32), 55LL);
    return r;
}
