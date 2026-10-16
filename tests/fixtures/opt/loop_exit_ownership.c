/* 多出口循环「出口所有权」逐形实测夹具（第一版：打印形态，期望值取自 gcc 实测）。
 *
 * 逐形覆盖 docs/wp_multi_exit_loop.md §2.1 的 S1–S15 与对抗样本 T1–T6：
 * 体内 return / return+break / 多个 break 合并 / continue-only / 旋转 do-while
 * （节点头即分支点）/ 无限循环 / break 落到零语句桩块 / 循环内 switch 的 case 出边 /
 * 多前驱 return 块。
 *
 * 20/21 两形钉的是**出口边的枚举口径**：`while(1)` 的 then 区走不到 latch，自然循环体
 * 不收它，而发射器会把它在到达边就地展开（连同它自己的 break 落点）。出口边若只按自然
 * 循环体枚举，就看不见 `then.0 -> while.end` 这类边 —— 落点既当不上续体也没被展开，
 * 它身上的 `return` 随整个落点丢掉（实测 -O2 返回残留在 %rax 的布尔值，
 * `shell.c` 的 stdin 读行因此首行即返回 0）。见 `loop_exit_region`。
 *
 * 判定以 gcc 为唯一权威：本文件先只打印，四档 stdout 与退出码逐字节对照 gcc。
 */
#include <stdio.h>

static int g_side;

/* 1 体内 return + 循环落出 */
static int f_early_ret(int n)
{
    for (int i = 0; i < n; i++) {
        if (i == 2) {
            return 7;
        }
    }
    return 1;
}

/* 2 return 与 break 同现 */
static int f_ret_break(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1) {
            return 9;
        }
        if (i == 3) {
            break;
        }
        s += i;
    }
    return s;
}

/* 3 两个 break 合并到同一个落点 */
static int f_two_breaks(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1) {
            break;
        }
        if (i == 2) {
            break;
        }
        s += i;
    }
    return s;
}

/* 4 只有 continue */
static int f_cont_only(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i % 2 == 0) {
            continue;
        }
        s += i;
    }
    return s;
}

/* 5 旋转后的 do-while：节点头本身就是分支点，体内 return */
static int f_do_ret(int n)
{
    int i = 0;
    do {
        i++;
        if (i == 3) {
            return 42;
        }
    } while (i < n);
    return -1;
}

/* 6 无限循环 + 节点头分支：return 与 break 并存 */
static int f_inf_ret_break(int n)
{
    int i = 0;
    for (;;) {
        if (i >= n) {
            return 5;
        }
        if (i == 2) {
            break;
        }
        i++;
    }
    return 6;
}

/* 7 break 落到零语句桩块，循环后面还有代码（两条分支都会被踩到：
   阈曾误写成 `s > 3`，实测那个条件下 `return 1` 在 n=0..6 全不可达 ⇒ 形同
   只测了一条边。改成 `s > 0` 后 n=0/1 走 `return 2`，n>=2 走 `return 1`。） */
static int f_break_then_after(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 2) {
            break;
        }
        s += i;
    }
    if (s > 0) {
        return 1;
    }
    return 2;
}

/* 8 侧出口自带语句再跳出（InlineOnEdge） */
static int f_side_break(int n)
{
    int s = 0;
    g_side = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1) {
            g_side = 101;
            break;
        }
        s += i;
    }
    return s + g_side;
}

/* 9 两个不同的纯 return 块 */
static int f_two_ret(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1) {
            return 9;
        }
        if (i == 2) {
            return 8;
        }
        s += i;
    }
    return s;
}

/* 10 return 携带循环变量 */
static int f_ret_loop_var(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 2) {
            return s;
        }
        s += i;
    }
    return s;
}

/* 11 节点头自带不可丢弃的写（while(k-- > 0)） */
static int f_while_kdec(int n)
{
    int k = n, s = 0;
    while (k-- > 0) {
        if (k == 2) {
            return 7;
        }
        s += k;
    }
    return s;
}

/* 12 纯无限循环，没有落出口 */
static int f_pure_inf(int n)
{
    for (;;) {
        if (n <= 0) {
            return 3;
        }
        n--;
    }
}

/* 13 嵌套：内层提前 return */
static int f_nested(int n)
{
    int acc = 0;
    for (int i = 0; i < n; i++) {
        for (int j = 0; j < i; j++) {
            if (j == 1) {
                return 77;
            }
        }
        acc += 1;
    }
    return acc;
}

/* 14 循环内 switch：case 的 break 出 switch、default 的 continue 出到节点头、
 *    另有 case 直接 return（出口边发射点的第 6 个） */
static int f_switch_loop(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        switch (i) {
        case 3:
            return 300;
        case 5:
            break;
        default:
            s += i;
            continue;
        }
        s += 10;
    }
    return s;
}

/* 15 多前驱的 return 块（|| 短路） */
static int f_multi_pred_ret(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1 || i == 2) {
            return 5;
        }
        s += i;
    }
    return s;
}

/* 16 return 携带侧出口写 + 循环后还有共享落点代码 */
static int f_ret_and_after(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1) {
            s = 9;
            return s;
        }
        if (i == 3) {
            return 4;
        }
        s += i;
    }
    return s;
}

/* 17 `while (k-- > 0)` 无侧出口（节点头带具名写，但头块就是唯一落出的条件块）。
   这一条钉的是「收窄不得把 C 里最常见的循环一并挡掉」：它与上面 while_kdec 的区别
   只有一个——没有体内 return。 */
static int f_kdec_plain(int n)
{
    int k = n, acc = 0;
    while (k-- > 0) {
        acc += k;
    }
    return acc;
}

/* 18 `while (k-- > 0)` + 体内 break（侧出口折到与循环条件同一个落点） */
static int f_kdec_break(int n)
{
    int k = n, s = 0;
    while (k-- > 0) {
        if (k == 2) {
            break;
        }
        s += k;
    }
    return s;
}

/* 19 `while (k-- > 0)` + break + return 同现（两个侧出口 + 头块具名写） */
static int f_kdec_both(int n)
{
    int k = n, s = 0;
    while (k-- > 0) {
        if (k == 3) {
            return 91;
        }
        if (k == 1) {
            break;
        }
        s += k;
    }
    return s + 1;
}

#define P(tag, fn)                                                                             \
    do {                                                                                       \
        printf("%s", tag);                                                                     \
        for (int k_ = 0; k_ <= 6; k_++) {                                                      \
            printf(" %d", fn(k_));                                                             \
        }                                                                                      \
        printf("\n");                                                                          \
    } while (0)

#define PS(tag, fn)                                                                            \
    do {                                                                                       \
        printf("%s", tag);                                                                     \
        for (int k_ = 0; k_ <= 6; k_++) {                                                      \
            printf(" %ld", (long)(fn(k_) - g_buf));                                            \
        }                                                                                      \
        printf("\n");                                                                          \
    } while (0)

/* 20 then 区被就地展开、break 落到**多前驱** Ret 块（整数值）：
 * `while(1)` 的 then 区走不到 latch ⇒ 自然循环体不收它，但发射器把它连同它自己的
 * 出口边一起打印在 `while` 之内。落点既没被数进共享出口（当不上续体）也没被就地展开时，
 * 它身上那句 `return v` 就整个丢掉，函数返回残留在 %rax 的短路 `&&` 布尔值。 */
static int f_then_break_ret(int n)
{
    int v = 1000 + n;
    int k = 0;
    while (1) {
        if (k == n) {
            if (k > 0 && v % 2 == 0) {
                v += 3;
            }
            break;
        }
        k++;
        if (k > 5) {
            break;
        }
    }
    return v;
}

/* 21 同一形的**指针**返回（`shell.c` 的 local_getline 形状：缓冲基址必须原样交回）。
 * 返回指针被丢成残留值时，`p - g_buf` 打印出一个巨大的负偏移，一眼可见。 */
static char g_buf[64];

static char *f_line_loop(int n)
{
    char *p = g_buf;
    int k = 0;
    while (1) {
        if (k == n) {
            if (k > 0 && p[k - 1] == 'q') {
                k--;
                if (k > 0 && p[k - 1] == '\r') {
                    k--;
                }
            }
            p[k] = '\0';
            break;
        }
        p[k] = 'q';
        k++;
        if (k > 5) {
            break;
        }
    }
    return p;
}

int main(void)
{
    P("early_ret      ", f_early_ret);
    P("ret_break      ", f_ret_break);
    P("two_breaks     ", f_two_breaks);
    P("cont_only      ", f_cont_only);
    P("do_ret         ", f_do_ret);
    P("inf_ret_break  ", f_inf_ret_break);
    P("break_after    ", f_break_then_after);
    P("side_break     ", f_side_break);
    P("two_ret        ", f_two_ret);
    P("ret_loop_var   ", f_ret_loop_var);
    P("while_kdec     ", f_while_kdec);
    P("pure_inf       ", f_pure_inf);
    P("nested         ", f_nested);
    P("switch_loop    ", f_switch_loop);
    P("multi_pred_ret ", f_multi_pred_ret);
    P("ret_and_after  ", f_ret_and_after);
    P("kdec_plain     ", f_kdec_plain);
    P("kdec_break     ", f_kdec_break);
    P("kdec_both      ", f_kdec_both);
    P("then_break_ret ", f_then_break_ret);
    PS("line_loop      ", f_line_loop);
    printf("done\n");
    return 0;
}
