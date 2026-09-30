/* -O3 循环展开必须替换**所有**形态里的归纳变量。
 *
 * `rewrite_stmt_ivars` 旧实现只处理 `Stmt::Expr` / `Stmt::Return` / `Stmt::Block`，
 * 于是循环体里一旦嵌套 `for` / `while` / `if` / `switch`，那份副本里的归纳变量就
 * **整体没被替换**：展开把外层 `for (int i = 0; i < 4; i++)` 换成「一条 `int i;`
 * 声明 + 4 份副本」，副本继续读那个从未被赋值的 `i`（恒 0）—— 实测
 * `for(i<4)for(j<4)s+=i*j;` 在 -O3 返回 0 而不是 36，与是否进 SSA 无关
 * （加 `goto` 的同形函数一样错）。
 *
 * 另一半是准入判据：`break`/`continue` 只在没被更内层循环/switch 屏蔽时绑定本循环。
 * 旧判据只看循环体的**直接**条目，于是 `for (i<4) { if (x) break; }` 会被展开成 4 份
 * 带 `break` 的语句 —— 展开后已没有本循环，那个 `break` 于是绑到更外层的目标，
 * 静默改变控制流。现在遇到这种形态直接放弃展开。
 *
 * 通过: exit 0（期望值取自 gcc -O0 参照）
 */

int a[4] = { 1, 2, 3, 4 };

/* 两层嵌套，体内同时用到两个归纳变量 */
int nest_2x2(void) {
    int s = 0;
    for (int i = 0; i < 2; i++)
        for (int j = 0; j < 2; j++)
            s = s + i * j;
    return s;
}

int nest_3x3(void) {
    int s = 0;
    for (int i = 0; i < 3; i++)
        for (int j = 0; j < 3; j++)
            s = s + i * j;
    return s;
}

int nest_4x4(void) {
    int s = 0;
    for (int i = 0; i < 4; i++)
        for (int j = 0; j < 4; j++)
            s = s + i * j;
    return s;
}

/* 外层归纳变量出现在**内层循环的条件**里：替换必须递归进 For 的 cond/step */
int nest_iv_in_cond(void) {
    int s = 0;
    for (int i = 1; i < 4; i++) {
        for (int j = 0; j < i; j++)
            s = s + j;
    }
    return s;
}

/* 归纳变量出现在 If / While / Switch 形态里 */
int nest_in_if(void) {
    int s = 0;
    for (int i = 0; i < 3; i++)
        for (int j = 0; j < 3; j++) {
            if (i > j)
                s = s + i * j;
        }
    return s;
}

int nest_in_while(void) {
    int s = 0;
    for (int i = 0; i < 3; i++)
        for (int j = 0; j < 3; j++) {
            int k = 0;
            while (k < i) {
                s = s + k + j;
                k = k + 1;
            }
        }
    return s;
}

int nest_in_switch(void) {
    int s = 0;
    for (int i = 0; i < 3; i++)
        for (int j = 0; j < 3; j++) {
            switch (i) {
            case 0:
                break;
            case 1:
                s = s + j;
                break;
            default:
                s = s + i * j;
                break;
            }
        }
    return s;
}

/* `break` 绑定到内层循环（被屏蔽）⇒ 允许展开；数组下标形态 */
int break_in_inner(void) {
    int s = 0;
    for (int i = 0; i < 3; i++)
        for (int j = 0; j < 3; j++) {
            if (j == 1)
                break;
            s = s + i * j;
        }
    return s;
}

int continue_in_inner(void) {
    int s = 0;
    for (int i = 0; i < 3; i++)
        for (int j = 0; j < 3; j++) {
            if (j == 1)
                continue;
            s = s + i * 10 + j;
        }
    return s;
}

/* 三层嵌套 */
int deep_3(void) {
    int s = 0;
    for (int i = 0; i < 2; i++)
        for (int j = 0; j < 2; j++)
            for (int k = 0; k < 2; k++)
                s = s + i * j * k;
    return s;
}

/* 归纳变量用作数组下标 */
int iv_as_subscript(void) {
    int s = 0;
    for (int i = 0; i < 4; i++)
        for (int j = 0; j < 2; j++)
            s = s + a[i] * j;
    return s;
}

int main(void) {
    if (nest_2x2() != 1)
        return 1;
    if (nest_3x3() != 9)
        return 2;
    if (nest_4x4() != 36)
        return 3;
    if (nest_iv_in_cond() != 4)
        return 4;
    if (nest_in_if() != 2)
        return 5;
    if (nest_in_while() != 12)
        return 6;
    if (nest_in_switch() != 9)
        return 7;
    if (break_in_inner() != 0)
        return 8;
    if (continue_in_inner() != 66)
        return 9;
    if (deep_3() != 1)
        return 10;
    if (iv_as_subscript() != 10)
        return 11;
    return 0;
}
