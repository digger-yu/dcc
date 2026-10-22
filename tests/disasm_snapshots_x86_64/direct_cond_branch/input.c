/* P13 阶段3: 条件直生分支的形状锁。
 *
 * 直生（一条 cmp/test 后直接 jcc，不许再出现 setcc + movzbq + test 的三元材料化）：
 *   有符号/无符号整数比较、窄类型（unsigned char）、指针与 0、三元、do-while。
 * 必须仍走材料化（本轮明确不接，形状一变就说明放行判据漂了）：
 *   浮点比较（ucomisd 的无序分支是 PF 与目标条件的复合）、位域读入的比较
 *   （通用路径尾部还要按宽度发掩码，那条 emit 会清掉 flags）、把比较结果当值用的
 *   嵌套比较与 `int r = (a == b)`。
 */
int j_signed(int a, int b) {
    if (a < b) return 1;
    if (a >= b) return 2;
    return 3;
}

unsigned j_unsigned(unsigned a, unsigned b) {
    if (a > b) return 1;
    if (a <= b) return 2;
    return 3;
}

int j_narrow(unsigned char c) {
    if (c > 200) return 1;
    return 0;
}

int j_ptr(const int *p) {
    if (p != 0) return 1;
    if (p == 0) return 2;
    return 3;
}

int j_ternary(int a, int b) { return a < b ? 11 : 22; }

int j_dowhile(int n) {
    int s = 0;
    do {
        s += n;
        n--;
    } while (n > 0);
    return s;
}

int j_float_still_materializes(double d, double e) {
    if (d < e) return 1;
    return 0;
}

struct bf {
    unsigned int a : 3;
};

int j_bitfield_still_materializes(struct bf *b) {
    if (b->a == 3) return 1;
    return 0;
}

int j_nested_value_still_materializes(int a, int b) {
    int t = (a < b) + 1;
    if (t > 3) return 1;
    return 0;
}

int j_bool_result_still_materializes(int a, int b) {
    int r = (a == b);
    if (r) return 1;
    return 0;
}
