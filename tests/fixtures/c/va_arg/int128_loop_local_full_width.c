/* 循环体内的 __int128 局部量必须整体 16 字节落槽，不能只写低半。
 *
 * 覆盖缺陷 D7：`__int128` 是 16 字节对象，dcc 的值模型把它按 `rdx:rax` 双寄存器
 * 交付。`Decl::Var` 的通用初始化分支用 `mov_to_stack` 存值，而它对任何 `sz >= 8`
 * 都只发一个 8 字节 store —— 高半（rdx）从未写进变量槽。后果不是"偶尔读到垃圾"
 * 而是可复现的错值：循环体内每次 `v >> 64` 读的都是同一个栈残值，实测三次迭代
 * 全部返回**最后一次**的高半（第一组 3,3,3 而非 1,2,3；第二组 6,6,6 而非 4,5,6）。
 *
 * 与 `va_arg` 的落点无关：命名参数占满 6 个 GP（`gp_offset == 48`，参数全走溢出区）
 * 与只占 1 个（两半都在寄存器保存区）症状完全相同，所以曾被归因为"11 命名参数
 * 特有"的那个跨 TU 夹具，其实与这里是同一个缺陷。
 *
 * 三个形态各自独立：
 *   - `decl_init`：`__int128 v = va_arg(...)`（本轮修复的点）
 *   - `pre_decl` ：先声明后 `v = va_arg(...)`（走赋值路径，作对照）
 *   - `wide`     ：11 个命名参数把游标推到 48
 *
 * 退出码只有 8 位可用，所以不逐参数占位（`64 << 2` 会被截断成 0 而伪装成全绿）；
 * 每个函数返回「首个读错的下标 + 1」，0 = 全对，三组各占 2 位。
 */
typedef __builtin_va_list va_list;

static __int128 mk(long long hi, long long lo) {
    unsigned __int128 u = (unsigned __int128)(unsigned long long) lo;
    u |= ((unsigned __int128)(unsigned long long) hi) << 64;
    return (__int128) u;
}
static long long lo_of(__int128 v) {
    return (long long) (unsigned __int128) v;
}
static long long hi_of(__int128 v) {
    return (long long)(v >> 64);
}

/* 从 `h0` 起连续取 3 个 __int128（第 q 个为 hi = h0+q、lo = (h0+q)*10），
 * 返回首个读错的下标 + 1（0 = 全对）。 */
#define CHECK3(ap, h0)                                    \
    for (int q = 0; q < 3; q++) {                         \
        __int128 v = __builtin_va_arg(ap, __int128);      \
        long long h = (long long) (h0) + q;               \
        if (lo_of(v) != h * 10 || hi_of(v) != h)          \
            return q + 1;                                 \
    }                                                     \
    return 0

/* 1 个命名参数 → 游标起点 8，两半都在寄存器保存区 */
static int decl_init(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    CHECK3(ap, 1);
}

/* 先声明后赋值：走 Assign 路径 */
static int pre_decl(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    __int128 v;
    for (int q = 0; q < 3; q++) {
        v = __builtin_va_arg(ap, __int128);
        if (lo_of(v) != (long long) (q + 4) * 10 || hi_of(v) != (long long) q + 4) {
            __builtin_va_end(ap);
            return q + 1;
        }
    }
    __builtin_va_end(ap);
    return 0;
}

/* 11 个命名参数占满 6 个 GP 寄存器 → gp_offset == 48，参数全在溢出区 */
static int wide(int a, int b, int c, int d, int e, int f, int g, int h, int i, int j, int k, ...) {
    (void) a; (void) b; (void) c; (void) d; (void) e; (void) f;
    (void) g; (void) h; (void) i; (void) j;
    va_list ap;
    __builtin_va_start(ap, k);
    CHECK3(ap, 7);
}

int main(void) {
    int r = 0;
    r |= decl_init(1, mk(1, 10), mk(2, 20), mk(3, 30));
    r |= pre_decl(1, mk(4, 40), mk(5, 50), mk(6, 60)) << 2;
    r |= wide(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, mk(7, 70), mk(8, 80), mk(9, 90)) << 4;
    return r;
}
