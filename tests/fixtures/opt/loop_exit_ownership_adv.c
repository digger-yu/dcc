/* 多出口循环「出口所有权」对抗样本（打印形态，判定以 gcc 实测为唯一权威）。
 *
 * 目的：钉出 `loop_exit_plan(...).expressible` 的**上界**。上一批（loop_exit_ownership.c）
 * 16 个形状四档全绿，但形状探针显示 probe 里的 21 个形状（含设计文档 §2.3 声称「应当
 * 仍被挡」的 T1–T6）**全部** expressible=true ⇒ 退路是变窄还是没了，必须靠更硬的形状回答。
 *
 * 本文件的形状都是「发射器可能无路可发」的构造：goto 出循环、break 落到带 phi 的汇聚块、
 * 同一循环两个不同的循环外汇聚块、无限循环 + 纯 break、嵌套循环内外同时出边。
 */
#include <stdio.h>

static int g;

/* A1 goto 跳出循环（循环外两个不同汇聚块：goto 目标 + 自然落出） */
static int a_goto_out(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 2) {
            goto out;
        }
        s += i;
    }
    s += 100;
out:
    return s;
}


/* A3 break 落到带 phi 的汇聚块（两次 break 前各写不同的值） */
static int a_break_phi(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1) {
            s = 11;
            break;
        }
        if (i == 2) {
            s = 22;
            break;
        }
        s += i;
    }
    return s;
}

/* A4 无限循环 + 只有 break（自然落出是唯一出口，且循环后仍有代码） */
static int a_inf_break(int n)
{
    int i = 0, s = 0;
    for (;;) {
        if (i > n) {
            break;
        }
        s += i;
        i++;
    }
    return s + 1;
}

/* A5 无限循环 + 两个不同的 return（没有落出口 ⇒ cont 必须为 None） */
static int a_inf_two_ret(int n)
{
    int i = 0;
    for (;;) {
        if (i > n) {
            return 1;
        }
        if (i == 2) {
            return 2;
        }
        i++;
    }
}

/* A6 外层 return + 内层 break（内层的落点就是外层的体内块） */
static int a_nested_inner_break(int n)
{
    int acc = 0;
    for (int i = 0; i < n; i++) {
        for (int j = 0; j < 4; j++) {
            if (j == 1) {
                break;
            }
            acc += 1;
        }
        if (i == 2) {
            return 500;
        }
    }
    return acc;
}

/* A7 嵌套：内层 break + 内层 return + 外层 break，外层循环后还有代码 */
static int a_nested_mixed(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        for (int j = 0; j < n; j++) {
            if (j == 1) {
                return 900;
            }
            if (j == 2) {
                break;
            }
            s += 1;
        }
        if (i == 4) {
            break;
        }
        s += i;
    }
    if (s > 10) {
        return 1;
    }
    return s;
}

/* A8 continue 制造第二个 latch，且尾部另有 break */
static int a_two_latches(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1) {
            continue;
        }
        if (i == 3) {
            break;
        }
        s += i;
    }
    return s;
}

/* A9 do-while 旋转 + break（节点头即分支点，落出与 break 两个候选） */
static int a_do_break(int n)
{
    int i = 0, s = 0;
    do {
        s += i;
        i++;
        if (i == 3) {
            break;
        }
    } while (i < n);
    return s + i;
}

/* A9b probe 里的 T5：do-while + break（节点头即分支点，落出与 break 两个候选） */
static int a_do_break_t5(int n)
{
    int i = 0;
    do {
        if (i == 2) {
            break;
        }
        i++;
    } while (i < n);
    return i;
}

/* A9c 同形状但侧出口是 return（对照：A9b 的判别因子是 break 而非「头块带写」） */
static int a_do_ret_t5(int n)
{
    int i = 0;
    do {
        if (i == 2) {
            return 77;
        }
        i++;
    } while (i < n);
    return i;
}

/* A10 循环内 switch：case 的 break 绑 switch，另一条路径 break 出循环 */
static int a_switch_break(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        switch (i) {
        case 1:
            break;
        case 3:
            s = 40;
            goto loop_out;
        default:
            s += i;
            break;
        }
        s += 1;
    }
loop_out:
    return s;
}

/* A11 侧出口是「多前驱的 return」（两条路径汇入同一个 return 块） */
static int a_shared_ret_block(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        int t;
        if (i == 1) {
            t = 5;
        } else if (i == 2) {
            t = 5;
        } else {
            t = 0;
        }
        if (t == 5) {
            return t;
        }
        s += i;
    }
    return s;
}

/* A12 循环体里有 while（外层节点头分支 + 内层落出，两个循环共享一个汇聚块） */
static int a_nested_while(int n)
{
    int s = 0;
    for (int i = 0; i < n; i++) {
        int j = 0;
        while (j < 3) {
            if (j == i) {
                s += 100;
                break;
            }
            j++;
        }
        if (s > 200) {
            return 3;
        }
        s += 1;
    }
    return s;
}

#define P(tag, fn)                                                                             \
    do {                                                                                       \
        printf("%s", tag);                                                                     \
        for (int k_ = 0; k_ <= 6; k_++) {                                                      \
            printf(" %d", fn(k_));                                                             \
        }                                                                                      \
        printf("\n");                                                                          \
    } while (0)

int main(void)
{
    P("goto_out       ", a_goto_out);
    P("break_phi      ", a_break_phi);
    P("inf_break      ", a_inf_break);
    P("inf_two_ret    ", a_inf_two_ret);
    P("nested_ibreak  ", a_nested_inner_break);
    P("nested_mixed   ", a_nested_mixed);
    P("two_latches    ", a_two_latches);
    P("do_break       ", a_do_break);
    P("do_break_t5    ", a_do_break_t5);
    P("do_ret_t5      ", a_do_ret_t5);
    P("switch_break   ", a_switch_break);
    P("shared_ret     ", a_shared_ret_block);
    P("nested_while   ", a_nested_while);
    printf("done %d\n", g);
    return 0;
}
