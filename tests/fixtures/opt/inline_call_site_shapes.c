/* 复合赋值 `x op= f(...)` 与声明初值 `T x = f(...)` 两个调用点形状必须能进内联决策，
 * 且展开后与未展开逐位同语义。
 *
 * 缺陷（方案 §4 批 3 的第 3、4 类形状）：`call_site_shape` 原来只认「整条语句就是
 * `x = f(...)` / `y = x = f(...)` / `if (f(...))`」，而 SQLite/Lua 里大量热形状是
 * `nVal += getVarint32(...)`、`int rc = f(...)` —— 它们在决策点之前就被 bail，
 * 连 DCC_INLINE_TRACE 的 REJECT 行都不打。
 *
 * 覆盖面（每段各钉一条判据的反面）：
 *   1. 可交换整型 `+= |= &= ^= *=`（parser 会把两侧对调，调用在左）；
 *   2. 不可交换 `-= /= %= >>= <<=`（调用在右）；
 *   3. 窄类型左值 `short s += f()`（内层 Binop 的类型是 int、Assign 的是 short，
 *      两个类型必须各自保留，不能重算）；
 *   4. 指针/数组左值 `*p += f()`、`a[1] -= f()`（读侧表达式原样搬回，与未展开时
 *      同一个「求值两次」形状，不新增也不消除）；
 *   5. 声明初值 `int v = f(...)`（展开成「空声明 + 赋值」，变量还在原作域）；
 *   6. 反向对照：`int v = f(...) + 1;`（调用嵌在更大表达式里，这条**不接**，值必须仍对）；
 *   7. 反向对照：调用点带副作用实参 `acc += f(i++)`（实参求值次数问题，这条**不接**）；
 *   8. callee 会改全局：`acc += f(k)` 里 f 改 `g_base`，而 `acc` 也参与 —— 读-改-写
 *      次序必须与未展开一致。
 *
 * 通过: exit 0（末行 `inline_call_site_shapes: OK`），四档与 gcc 输出一致。
 */

#include <stdio.h>

static int fails;
static volatile int seed;
static int g_base;

#define EXPECT(got, want, name)                                                                  \
    do {                                                                                         \
        long long got_ = (long long)(got);                                                       \
        long long want_ = (long long)(want);                                                     \
        if (got_ != want_) {                                                                     \
            printf("FAIL %s: got %lld want %lld\n", (name), got_, want_);                       \
            fails++;                                                                             \
        }                                                                                       \
    } while (0)

/* 带控制流的小助手：专属两条（单 return 体 / 平坦体 + 语句调用点）都接不住它，
 * 只有通用成本路能接 —— 也就是本批要钉的那条通道。 */
static int help_add(int k) {
    int r = k;
    if (k & 1) {
        r = k + 3;
    } else {
        if (k == 12) {
            return 100;
        }
        r = k - 1;
    }
    while (r > 50) {
        r -= 50;
    }
    return r;
}

static int help_mul(int k) {
    if (k < 0) {
        return -k;
    }
    return k * 3;
}

/* 8) callee 会改全局：读-改-写次序必须与未展开时一致 */
static int help_touch(int k) {
    g_base += 7;
    if (g_base > 100) {
        g_base = 1;
    }
    return k + g_base;
}

static int commutative_ops(int k) {
    int acc = 10;
    acc += help_add(k);
    acc |= help_add(k + 1);
    acc &= 0777;
    acc ^= help_add(k);
    acc *= help_mul(1);
    return acc;
}

static int non_commutative_ops(int k) {
    int acc = 100;
    acc -= help_add(k);
    acc /= help_mul(2);
    acc %= help_add(4);
    acc <<= 2;
    acc >>= help_mul(1);
    return acc;
}

static int narrow_target(int k) {
    short s = 3;
    s += help_add(k);
    s -= 1;
    return (int)s;
}

static int arr[4];

static int pointer_and_array(int k) {
    int i;
    int *p = &arr[2];
    arr[0] = 5;
    arr[1] = 7;
    arr[2] = 9;
    *p += help_add(k);
    arr[1] -= help_add(k + 2);
    i = *p + arr[1];
    return i;
}

static int decl_init(int k) {
    int v = help_add(k); /* 第 5 段：这条初值可展开 */
    int w = help_mul(v);
    return v + w;
}

static int decl_init_nested(int k) {
    int v = help_add(k) + 1; /* 反向对照：调用嵌在 Binop 里，不接 */
    return v;
}

static int arg_impure(int k) {
    int acc = 0;
    acc += help_add(k); /* 实参是纯变量，这条可展开 */
    seed += 1;
    return acc + (int)seed;
}

static int touch_order(int k) {
    g_base = 0;
    int acc = 1;
    acc += help_touch(k);
    acc += help_touch(k + 1);
    return acc * 10 + g_base;
}

int main(void) {
    /* 期望值逐条来自 gcc 臂（-O0 与 -O2 同值），不是手算 */
    g_base = 0;
    seed = 0;
    EXPECT(commutative_ops(4), 42, "commutative_ops(4)");
    EXPECT(non_commutative_ops(4), 0, "non_commutative_ops(4)");
    EXPECT(narrow_target(4), 5, "narrow_target(4)");
    EXPECT(pointer_and_array(4), 14, "pointer_and_array(4)");
    EXPECT(decl_init(4), 12, "decl_init(4)");
    EXPECT(decl_init_nested(4), 4, "decl_init_nested(4)");
    EXPECT(arg_impure(4), 4, "arg_impure(4)");
    EXPECT(touch_order(4), 324, "touch_order(4)");
    /* 同一批函数换个入参再跑一遍（展开后形参临时量不得在两次调用间串味） */
    g_base = 0;
    seed = 0;
    EXPECT(commutative_ops(6), 30, "commutative_ops(6)");
    EXPECT(decl_init(6), 20, "decl_init(6)");
    EXPECT(touch_order(6), 364, "touch_order(6)");
    /* 12 那一支走 help_add 的中段 return */
    EXPECT(decl_init(12), 400, "decl_init(12) 走中段 return");
    EXPECT(narrow_target(12), 102, "narrow_target(12)");

    if (fails) {
        printf("inline_call_site_shapes: %d FAIL\n", fails);
        return 1;
    }
    printf("inline_call_site_shapes: OK\n");
    return 0;
}
