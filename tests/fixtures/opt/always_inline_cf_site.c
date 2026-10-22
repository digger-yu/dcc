/* always_inline 的带控制流助手必须真的被展开（而不是留一个 call）。
 *
 * 缺陷（阶段 6 §15.8-4 的「三条路互斥」）：`always_inline` 的专属两条路一条要
 * 「体恰好是一条 `return 表达式;`」，一条要「体是 [声明]* [表达式语句]* [return?]
 * 且调用点是表达式语句/尾位置」；而成本模型那条通用路又把 `always_inline` 整体拒掉
 * （「那是专属路的事」）。于是**带控制流的** always_inline 助手在 `r = f(...)` 这种
 * 调用点上三条路全关 —— 程序员明说了要展开，编译器反而一个都不给（SQLite btree 的
 * `allocateSpace` 就是这个形状）。
 *
 * 覆盖面（每段各钉一条展开重写容易丢的东西）：
 *   1. 赋值调用点 + 体内 if/else + 中段 return；
 *   2. **同一 callee 多个调用点**：形参与局部改名必须逐份独立（共用一份就是串味）；
 *   3. 实参是 caller 里会被体改写的变量（求值次序）；
 *   4. callee 里带 `while` 循环；
 *   5. callee 里带 `goto`+标签，caller 自己也有同名标签 ⇒ 标签必须加前缀；
 *   6. always_inline 链：callee 又调一个 always_inline（递归深度不是 1）；
 *   7. `if (f(...))` 条件调用点；
 *   8. 链式赋值 `y = x = f(...)`。
 *
 * 通过: exit 0（末行 `always_inline_cf_site: OK`），四档与 gcc 输出一致。
 */

#include <stdio.h>

static int fails;

#define EXPECT(got, want, name)                                                                  \
    do {                                                                                         \
        long long got_ = (long long)(got);                                                       \
        long long want_ = (long long)(want);                                                     \
        if (got_ != want_) {                                                                     \
            printf("FAIL %s: got %lld want %lld\n", (name), got_, want_);                       \
            fails++;                                                                             \
        }                                                                                       \
    } while (0)

/* 1/4：if/else + 中段 return + while */
static __attribute__((always_inline)) int clamp_step(int x) {
    int n;
    if (x > 0) {
        n = x + 1;
    } else {
        if (x == -7) {
            return 99; /* 中段 return */
        }
        n = x - 1;
    }
    while (n > 100) {
        n -= 100;
    }
    return n;
}

/* 2：同一 callee 的第二、第三个调用点 */
static int two_sites(int a, int b) {
    int r;
    int s;
    r = clamp_step(a);
    s = clamp_step(b);
    return r * 1000 + s;
}

/* 3：目的地与实参同名（展开后的代入不得把「先读后写」颠倒） */
static int arg_reuse(int x) {
    int y;
    y = x;
    y = clamp_step(y);
    return y;
}

/* 5：callee 有 goto/标签，caller 也有同名的 */
static __attribute__((always_inline)) int with_label(int x) {
    int r = 0;
    if (x > 2) {
        goto L_add;
    }
    r = x;
    goto L_end;
L_add:
    r = x * x;
L_end:
    return r + 1;
}

static int caller_with_same_label(int x) {
    int acc;
    acc = with_label(x);
    if (acc > 5) {
        goto L_end; /* 与 callee 里那个标签同名 */
    }
    acc += 1;
L_end:
    return acc;
}

/* 6：always_inline 链 */
static __attribute__((always_inline)) int inner_cf(int x) {
    if (x & 1) {
        return x + 7;
    }
    return x - 7;
}
static __attribute__((always_inline)) int outer_cf(int x) {
    int r;
    r = inner_cf(x);
    if (r < 0) {
        r = 0;
    }
    return r;
}

/* 7/8：条件调用点与链式赋值调用点 */
static int cond_and_chain(int x) {
    int a;
    int b;
    a = 0;
    b = 0;
    if (clamp_step(x)) {
        a = 1;
    }
    b = a = clamp_step(x + 1);
    return a + b;
}

/* 9：反向对照放在发射形态用例里（那一臂只比 dcc 自己）：递归的 always_inline 不得展开。
 * 本夹具不带它 —— gcc 对「递归 + always_inline」是**硬错误**（`inlining failed in call to
 * ‘always_inline’ ‘rec_sum’: function not considered for inlining`），放进四档对照就过不了。*/

/* ── 10-13：目的地形状上的类型判据（阶段 6 批 3 的 Dest 缺陷）─────────────────
 * 结果临时量的声明类型是 callee 的返回类型，而以前
 *   * `Dest::Cond` 硬贴 `Type::Int` ⇒ `if (dsel(k))` 按 32 位整数判一个 double 的位
 *     模式（1.0 的低 32 位是 0）⇒ 真被判成假（实测 -O2 返 2，-O0 与 gcc 返 1）；
 *   * `Dest::Assign` 贴左值类型 ⇒ `i = dval(k)` 少了那次 cvttsd2si，double 的位模式
 *     被当 int 存（实测 -O2 得 0，-O0 与 gcc 得 3）；
 *   * `Dest::AssignChain` 贴内层左值类型给两边 ⇒ 外层拿到的是未转换的值。
 *     C 的规则是「赋值表达式的值就是已经转换过的那个值」：`y = x = f()` 里 f() 返
 *     3.5 时 x 是 3、y 也必须是 3.0 而不是 3.5。
 */
static __attribute__((always_inline)) double dsel(long k) {
    if (k) {
        return 1.0;
    }
    return 0.0;
}
static __attribute__((always_inline)) double dval(long k) {
    if (k > 0) {
        return 3.5;
    }
    return -1.5;
}
static int truth_of_double(long k) {
    if (dsel(k)) {
        return 1;
    }
    return 2;
}
static int assign_double_to_int(long k) {
    int i;
    i = 0;
    i = dval(k);
    return i;
}
static int assign_double_to_double(long k) {
    double d;
    d = 0.0;
    d = dval(k);
    /* +0.5 后再取整：3.5→4、-1.5→-1，证明那次读真的是 double */
    return (int)(d + 0.5);
}
static int chain_mixed(long k) {
    int x;
    double y;
    y = x = dval(k);
    /* 外层必须拿到内层转换后的值：3.0 + 3*100 = 303（不是 3.5+300=303.5） */
    return (int)(y + (double)x * 100.0);
}
static int g_a;
static int g_b;
static __attribute__((always_inline)) int *psel(int k) {
    if (k) {
        return &g_a;
    }
    return &g_b;
}
static int truth_of_pointer(int k) {
    if (psel(k)) {
        return 1;
    }
    return 2;
}
static int which_pointer(int k) {
    int *p;
    p = psel(k);
    return p == &g_a;
}
static __attribute__((always_inline)) long lsel(int k) {
    if (k) {
        return 0x12345678L;
    }
    return -2L;
}
static int narrow_long_to_short(int k) {
    short s;
    s = 0;
    s = lsel(k);
    return s;
}

int main(void) {
    EXPECT(clamp_step(5), 6, "clamp_step(5)");
    EXPECT(clamp_step(-7), 99, "clamp_step(-7)");
    EXPECT(clamp_step(-200), -201, "clamp_step(-200)");
    EXPECT(clamp_step(250), 51, "clamp_step(250)"); /* 251 递推两个 100 */
    EXPECT(two_sites(5, -7), 6099, "two_sites(5,-7)");
    EXPECT(two_sites(-200, 250), -200949, "two_sites(-200,250)");
    EXPECT(arg_reuse(9), 10, "arg_reuse(9)");
    EXPECT(caller_with_same_label(3), 10, "caller_with_same_label(3)");
    EXPECT(caller_with_same_label(1), 3, "caller_with_same_label(1)");
    EXPECT(outer_cf(4), 0, "outer_cf(4)");
    EXPECT(outer_cf(10), 3, "outer_cf(10)"); /* 10 是偶数 → 10-7 */
    EXPECT(cond_and_chain(5), 14, "cond_and_chain(5)");
    /* 10-13：判据的值由 gcc 臂给出 */
    EXPECT(truth_of_double(1), 1, "truth_of_double(1)");
    EXPECT(truth_of_double(0), 2, "truth_of_double(0)");
    EXPECT(assign_double_to_int(1), 3, "assign_double_to_int(1)");
    EXPECT(assign_double_to_int(0), -1, "assign_double_to_int(0)");
    EXPECT(assign_double_to_double(1), 4, "assign_double_to_double(1)");
    EXPECT(assign_double_to_double(0), -1, "assign_double_to_double(0)");
    EXPECT(chain_mixed(1), 303, "chain_mixed(1)");
    EXPECT(chain_mixed(-1), -101, "chain_mixed(-1)"); /* -1.5 → x=-1、y=-1.0 */
    EXPECT(truth_of_pointer(1), 1, "truth_of_pointer(1)");
    EXPECT(truth_of_pointer(0), 1, "truth_of_pointer(0)");
    EXPECT(which_pointer(1), 1, "which_pointer(1)");
    EXPECT(which_pointer(0), 0, "which_pointer(0)");
    EXPECT(narrow_long_to_short(1), 0x5678, "narrow_long_to_short(1)");
    EXPECT(narrow_long_to_short(0), -2, "narrow_long_to_short(0)");

    if (fails) {
        printf("always_inline_cf_site: %d FAIL\n", fails);
        return 1;
    }
    printf("always_inline_cf_site: OK\n");
    return 0;
}
