/* 整型加宽必须按**源类型**的符号性归一（unsigned → 更宽类型 = 零扩展）。
 *
 * 症状（实测旧 HEAD，**四档全错**，与 gcc 不一致）：
 *   unsigned long f(int v){ return 100000000000UL + (unsigned)v; }   // v=-1
 *   gcc  : 104294967295      dcc : 99999999999
 *
 * 三个失守点分别在不同层（逐个上下文实测反汇编得出，不是猜）：
 *   * 寄存器操作数路径一直是**对的**：`return (unsigned)v;`、`(unsigned)v + 1`、
 *     `if ((unsigned)v > 3u)` 都会发 `movl %eax, %eax`（cast.rs 的 (Int, UInt) 同宽臂）。
 *   * 失守一：赋值/存储到更宽目标。`z = (unsigned)v;` 与 `*p = (unsigned)v;` 只剩
 *     `movsxd` + 一次 64 位存，零扩展那一条整个不见 —— 目标类型是 `unsigned long`，
 *     「按目标符号性决定要不要归一」的那个判据（`is_signed_int(target_ty)`）在这里
 *     天然得出不需要动作的结论，而源表达式自己的 (unsigned) 被整层跳过。
 *     修法：标量写白名单 `scalar_load_shape` 不再剥「同宽但换符号性」的 cast。
 *   * 失守二：操作数从**帧槽**装载时走的是窄装载快道，按 lvalue 自己的类型扩展，
 *     同样不经过同宽 cast 臂（同一个表达式从 `%edi` 取就正常，从 `-64(%r13)` 取就错）。
 *   * 失守三：归一那条 `mov dst32, dst32` **发出来了却被发射层窥孔删掉**。窥孔把
 *     `movsxd r64, DWORD PTR [mem]`（符号扩展装载）与 32 位零扩展装载归成同一类
 *     `LoadR32`，其含义是「高 32 位已为零」，于是紧随其后的自反零扩展被当冗余 ——
 *     符号扩展装载的目的高 32 位是符号位，这个前提对它不成立。修法是单独立类
 *     `LoadS32`。这条最难查：`DCC_BLOCKOPT=0` 下依旧错（它不是块内优化），而
 *     `gen_expr_cast` 确实被调到了（临时诊断确认过）。
 *
 * 覆盖面：显式 cast / 经过一个 unsigned 局部 / 进函数实参 / 数组元素 /
 * unsigned short 与 unsigned char 的零扩展 / 反向对照 —— 有符号源加宽必须
 * 仍做符号扩展。通过: exit 0（末行 `int_widen_signedness: OK`），四档与 gcc 一致。
 */

#include <stdio.h>

static int fails;
static volatile int neg_one = -1;
static volatile int pos_val = 7;

#define EXPECT(expr, want, name)                                                                 \
    do {                                                                                         \
        long long got_ = (long long)(expr);                                                        \
        long long want_ = (long long)(want);                                                       \
        if (got_ != want_) {                                                                      \
            printf("FAIL %s: got %lld want %lld\n", (name), got_, want_);                          \
            fails++;                                                                              \
        }                                                                                         \
    } while (0)

/* --- 形态 1：cast 直接进 64 位表达式（错值的那条） --- */
static unsigned long widen_inline_cast(void) {
    int v = neg_one;
    return 100000000000UL + (unsigned)v;
}

/* --- 形态 2：先落进一个 unsigned 局部，再加宽 --- */
static unsigned long widen_via_unsigned_local(void) {
    int v = neg_one;
    unsigned u = (unsigned)v;
    unsigned long r = u;
    return r;
}

/* --- 形态 3：unsigned 局部参与 64 位加法 --- */
static unsigned long widen_local_in_expr(void) {
    int v = neg_one;
    unsigned u = (unsigned)v;
    return 100000000000UL + u;
}

/* --- 形态 4：unsigned 作为实参进 64 位形参 --- */
static unsigned long sink(unsigned long x) {
    return x;
}
static unsigned long widen_via_argument(void) {
    int v = neg_one;
    return sink((unsigned)v);
}

/* --- 形态 5：unsigned 数组元素加宽 --- */
static unsigned long widen_array_elem(void) {
    unsigned a[2];
    a[0] = (unsigned)neg_one;
    a[1] = 1u;
    return 4000000000000UL + a[0];
}

/* --- 形态 6：unsigned short / unsigned char 加宽（都是零扩展） --- */
static unsigned long widen_ushort(void) {
    volatile unsigned short s = 0xFFFFu;
    return 100000000000UL + s;
}
static unsigned long widen_uchar(void) {
    volatile unsigned char c = 0xFFu;
    return 100000000000UL + c;
}

/* --- 反向对照 1：有符号 char 加宽必须符号扩展 --- */
static long widen_schar_signed(void) {
    volatile signed char c = -5;
    return 1000000000000L + c;
}

/* --- 反向对照 2：有符号 short 加宽必须符号扩展 --- */
static long widen_short_signed(void) {
    volatile short s = -6;
    return 1000000000000L + s;
}

/* --- 反向对照 3：int 加宽到 long 保持符号 --- */
static long widen_int_signed(void) {
    int v = neg_one;
    return 1000000000000L + v;
}

/* --- 反向对照 4：正值 unsigned 加宽不受影响（防止把修复做反） --- */
static unsigned long widen_positive_unchanged(void) {
    int v = pos_val;
    return 100000000000UL + (unsigned)v;
}

/* --- 加宽后再窄化回去，两个方向的判据要同时成立 --- */
static unsigned long widen_then_narrow(void) {
    int v = neg_one;
    unsigned long w = (unsigned)v;
    unsigned back = (unsigned)w;
    return back;
}

int main(void) {
    EXPECT(widen_inline_cast(), 104294967295LL, "widen_inline_cast");
    EXPECT(widen_via_unsigned_local(), 4294967295LL, "widen_via_unsigned_local");
    EXPECT(widen_local_in_expr(), 104294967295LL, "widen_local_in_expr");
    EXPECT(widen_via_argument(), 4294967295LL, "widen_via_argument");
    EXPECT(widen_array_elem(), 4004294967295LL, "widen_array_elem");
    EXPECT(widen_ushort(), 100000065535LL, "widen_ushort");
    EXPECT(widen_uchar(), 100000000255LL, "widen_uchar");
    EXPECT(widen_schar_signed(), 999999999995LL, "widen_schar_signed");
    EXPECT(widen_short_signed(), 999999999994LL, "widen_short_signed");
    EXPECT(widen_int_signed(), 999999999999LL, "widen_int_signed");
    EXPECT(widen_positive_unchanged(), 100000000007LL, "widen_positive_unchanged");
    EXPECT(widen_then_narrow(), 4294967295LL, "widen_then_narrow");

    if (fails) {
        printf("int_widen_signedness: %d FAIL\n", fails);
        return 1;
    }
    printf("int_widen_signedness: OK\n");
    return 0;
}
