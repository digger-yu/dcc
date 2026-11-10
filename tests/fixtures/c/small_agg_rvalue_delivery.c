/* 报告 §5.15：小聚合（≤8 B）**按地址交付**的右值被当成寄存器数据存槽。
 *
 * 值模型有两条轴，缺一条就错：
 *   类型轴 `value_is_whole_copy` —— 对象装不进一个寄存器（>8 B 或运行时尺寸）；
 *   交付轴 `rvalue_is_buffer_addr` —— 右值求值后 rax 里是**缓冲地址**而不是数据
 *     （复合字面量、语句表达式、整块拷贝过的链式赋值、物化过的聚合三元）。
 * 修复前交付轴被尺寸轴挡在前面，于是 2 B 结构的 `g = ({ struct s t; …; t; })`
 * 只走标量 store：把物化槽的**地址**写进 g（gcc 给 3,4；dcc 给垃圾）。
 *
 * 这里的每一条断言都取自 gcc 在 -O0/-O2 下的实测值；退出码逐位对应一条轴：
 *   1 语句表达式→全局      2 →局部        3 →成员      4 →下标
 *   5 →解引用              6 链式赋值     7 调用实参   8 返回（≤8 B）
 *   9 返回（16 B）        10 复合字面量（对照，修复前就对）
 *  11 2 B 结构按值传参（对照：SysV 单寄存器交付，修复前就对，防误伤）
 * 另：`checks` 计数把每一处赋值都变成“读回来还要用”，避免被当成死代码消掉。
 */
struct tiny {
    char a, b;
};

struct quad {
    int a, b;
};

struct wide {
    long a, b;
};

static struct tiny g_tiny;
static struct quad g_quad;
static int checks;

static struct tiny ret_tiny_stmt(void) {
    return ({ struct tiny y; y.a = 21; y.b = 22; y; });
}

static struct wide ret_wide_stmt(void) {
    return ({ struct wide y; y.a = 23; y.b = 24; y; });
}

static int take_tiny(struct tiny v) {
    checks++;
    return v.a * 10 + v.b;
}

/* 2 B 结构按值传参 + 静态目标赋值：SysV 把整个对象放在一个通用寄存器里，
 * 一次 2 B store 就是**对的**（gcc.c-torture/execute/20040707-1.c 的形状）。
 * 这条对照防的是“把一切聚合赋值都改成整块拷贝”那种过度修正。 */
static struct tiny s1;
static void copy_by_reg(struct tiny s) {
    s1 = s;
    checks++;
}

int main(void) {
    int rc = 0;

    /* 1. 语句表达式 → 全局 */
    g_tiny = ({ struct tiny t; t.a = 3; t.b = 4; t; });
    if (g_tiny.a != 3 || g_tiny.b != 4) rc |= 1;

    /* 2. 语句表达式 → 局部 */
    struct tiny l;
    l = ({ struct tiny t; t.a = 5; t.b = 6; t; });
    if (l.a != 5 || l.b != 6) rc |= 2;

    /* 3. → 成员 */
    struct outer {
        struct tiny in;
        int k;
    } o;
    o.in = ({ struct tiny t; t.a = 7; t.b = 8; t; });
    if (o.in.a != 7 || o.in.b != 8) rc |= 3;

    /* 4. → 下标 */
    struct tiny arr[3];
    arr[1] = ({ struct tiny t; t.a = 9; t.b = 10; t; });
    if (arr[1].a != 9 || arr[1].b != 10) rc |= 4;

    /* 5. → 解引用 */
    struct tiny *p = &arr[2];
    *p = ({ struct tiny t; t.a = 11; t.b = 12; t; });
    if (p->a != 11 || p->b != 12) rc |= 5;

    /* 6. 链式赋值：内层赋值表达式交付的是目标地址 */
    struct tiny c1, c2;
    c1 = (c2 = ({ struct tiny t; t.a = 13; t.b = 14; t; }));
    if (c1.a != 13 || c1.b != 14) rc |= 6;
    if (c2.a != 13 || c2.b != 14) rc |= 6;

    /* 7. 调用实参 */
    if (take_tiny(({ struct tiny t; t.a = 15; t.b = 16; t; })) != 166) rc |= 7;

    /* 8. 返回（≤8 B，SysV 单寄存器） */
    struct tiny r8 = ret_tiny_stmt();
    if (r8.a != 21 || r8.b != 22) rc |= 8;

    /* 9. 返回（16 B，rax:rdx） */
    struct wide r16 = ret_wide_stmt();
    if (r16.a != 23 || r16.b != 24) rc |= 9;

    /* 10. 复合字面量对照（修复前就对，防回退） */
    g_quad = (struct quad){17, 18};
    if (g_quad.a != 17 || g_quad.b != 18) rc |= 10;

    /* 11. 2 B 结构按值传参 → 静态目标（单寄存器交付，不得被整块拷贝规则误伤） */
    struct tiny arg;
    arg.a = 19;
    arg.b = 20;
    copy_by_reg(arg);
    if (s1.a != 19 || s1.b != 20) rc |= 11;

    /* 所有读取都参与结果，避免被当成死代码消掉 */
    checks += (int)g_tiny.a + (int)l.b + (int)o.in.a + arr[1].b + p->a;
    return rc == 0 && checks == 7 ? 0 : (rc | 0x100);
}
