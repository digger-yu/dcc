/* SSA 读—改—写期的位域事实：位域没有自己的存储地址，`&obj.bf` 给的是**容器**地址。
 *
 * `Builder::lower_mem_rmw`（`++x` / `--x` / `x op= y` 共用的那条机器）先对左值取址、
 * 再发 Load—Bin—Store。对普通标量左值正确，对位域左值则是静默误编译：整容器被当
 * 成一个值来加 1，于是进位**溢出到相邻位域**，而位域的截断（结果要归约到位域类型）
 * 也整个丢掉。实测 -O2/-O3 下
 *   `struct {unsigned b:3; unsigned pad:29;} v; v.b = 7; v.pad = 5; v.b += 1;`
 * 得到 `b=0 pad=6`（gcc 是 `pad=5`），`arr[i].n++` 同一条错（gcc-torture pr55750
 * 在 -O2 里 abort）。-O0/-O1 走 AST 通道，本来就对。
 *
 * 判据与 codegen 侧 `expr_bitfield_width` 一致（字段 `bit_width` 非空即位域）；命中
 * 后按既有政策整函数留在 AST 通道 —— 正确但不优化，绝不按容器宽度做 RMW。
 *
 * 夹具覆盖的形态：具名对象 / 指针 / 数组元素（下标运行时才知道）三种基址 ×
 * `+= ++ -- --prefix -= *= <<= &= |=` 六种改写 × 无符号与有符号位域（有符号要
 * 回绕成负数）× RMW 的表达值是否已归约到位域类型 × 相邻位域是否被误伤。
 *
 * 通过: exit 0（期望值取自 gcc -O0 参照，逐条与 gcc 各档位对拍）
 */

struct U3 {
    unsigned b : 3;
    unsigned pad : 29;
};
struct S4 {
    int sb : 4;
    int rest : 28;
};
struct Two {
    unsigned lo : 4;
    unsigned hi : 4;
};

struct U3 gu;
struct Two gt;
struct U3 ga[2];

/* 具名对象 + 复合赋值：结果值归约到 3 位，相邻 pad 不得被进位污染 */
int t_named_compound(void) {
    gu.b = 7;
    gu.pad = 5;
    int r = (gu.b += 1);
    if (gu.b != 0)
        return 101;
    if (r != 0)
        return 102;
    if (gu.pad != 5)
        return 103;
    return 0;
}

/* 前缀：结果值 = 归约后的新值；后缀：结果值 = 旧值 */
int t_pre_post(void) {
    gu.b = 7;
    gu.pad = 2;
    if ((++gu.b) != 0)
        return 111;
    if (gu.b != 0)
        return 112;
    if ((gu.b++) != 0)
        return 113;
    if (gu.b != 1)
        return 114;
    if ((--gu.b) != 0)
        return 115;
    if ((gu.b--) != 0)
        return 116;
    if (gu.b != 7) /* 0 - 1 在 3 位无符号里回绕 */
        return 117;
    if (gu.pad != 2)
        return 118;
    return 0;
}

/* 指针基址 (`p->b += 1`) */
int t_arrow_compound(void) {
    struct U3 *p = &gu;
    gu.b = 6;
    gu.pad = 1;
    p->b += 2;
    if (p->b != 0)
        return 121;
    if (gu.pad != 1)
        return 122;
    p->b -= 1;
    if (p->b != 7)
        return 123;
    if (gu.pad != 1)
        return 124;
    return 0;
}

/* 数组元素基址（pr55750 的形状：`arr[i].n++`），下标由参数决定 */
int t_array_index(unsigned i) {
    ga[0].b = 7;
    ga[0].pad = 5;
    ga[1].b = 0;
    ga[1].pad = 3;
    ga[i].b += 1;
    ga[1 - i].b++;
    if (i == 0) {
        if (ga[0].b != 0 || ga[0].pad != 5)
            return 131;
        if (ga[1].b != 1 || ga[1].pad != 3)
            return 132;
    } else {
        if (ga[1].b != 1 || ga[1].pad != 3)
            return 133;
        if (ga[0].b != 0 || ga[0].pad != 5)
            return 134;
    }
    if (ga[0].pad != 5 || ga[1].pad != 3)
        return 136;
    return 0;
}

/* 有符号位域：`++` 到边界要回绕成负数，且 rest 不动。
 * 结果值（不只是写回内存的那几位）必须按位域类型做符号扩展 —— gcc 给 -8/-7/-1，
 * 只 `and` 不 `sar` 的实现给 8/9/15。 */
int t_signed_wrap(void) {
    struct S4 v;
    v.sb = 7;
    v.rest = 3;
    if ((++v.sb) != -8)
        return 141;
    if (v.sb != -8)
        return 142;
    if (v.rest != 3)
        return 143;
    v.sb = -8;
    v.sb -= 1;
    if (v.sb != 7)
        return 144;
    if (v.rest != 3)
        return 145;
    v.sb = -8;
    v.rest = 3;
    if ((v.sb--) != -8) /* 后缀的结果是旧值 -8 */
        return 146;
    if (v.sb != 7 || v.rest != 3)
        return 147;
    v.sb = 5;
    v.rest = 3;
    if ((v.sb *= 3) != -1) /* 15 归约到 4 位有符号 = -1 */
        return 148;
    if (v.sb != -1 || v.rest != 3)
        return 149;
    return 0;
}

/* 同容器内的第二个位域：RMW 只能改自己那几位 */
int t_two_fields(void) {
    gt.lo = 15;
    gt.hi = 1;
    gt.lo += 1;
    if (gt.lo != 0)
        return 151;
    if (gt.hi != 1)
        return 152;
    gt.hi += 1;
    if (gt.hi != 2 || gt.lo != 0)
        return 153;
    return 0;
}

/* 位移/按位/乘法复合赋值同样归约到位域宽度 */
int t_other_ops(void) {
    gu.b = 2;
    gu.pad = 4;
    gu.b |= 4;
    if (gu.b != 6)
        return 161;
    gu.b <<= 1;
    if (gu.b != 4) /* 12 截成 3 位 */
        return 162;
    gu.b &= 3;
    if (gu.b != 0)
        return 163;
    gu.b = 5;
    gu.b *= 3;
    if (gu.b != 7) /* 15 截成 3 位 */
        return 164;
    if (gu.pad != 4)
        return 165;
    return 0;
}

/* 未展开成 `+=` 的同形写法（parser 会给出一条 Binop 左值拷贝）也必须走位域路径 */
int t_explicit_add(void) {
    gu.b = 7;
    gu.pad = 6;
    gu.b = gu.b + 1;
    if (gu.b != 0)
        return 171;
    if (gu.pad != 6)
        return 172;
    return 0;
}

/* RMW 的结果参与后续计算 */
int t_result_flows(int k) {
    gu.b = 6;
    gu.pad = 7;
    int r = (gu.b += k);
    return r + gu.b * 10 + gu.pad * 100;
}

int main(void) {
    if (t_named_compound() != 0)
        return 1;
    if (t_pre_post() != 0)
        return 2;
    if (t_arrow_compound() != 0)
        return 3;
    if (t_array_index(0) != 0)
        return 4;
    if (t_array_index(1) != 0)
        return 5;
    if (t_signed_wrap() != 0)
        return 6;
    if (t_two_fields() != 0)
        return 7;
    if (t_other_ops() != 0)
        return 8;
    if (t_explicit_add() != 0)
        return 9;
    /* r = 归约后的新值 = 0，b 亦为 0，pad 仍是 7 */
    if (t_result_flows(2) != 700)
        return 10;
    /* b: 6+9 = 15 → 7（15 截成 3 位），r 同为 7 */
    if (t_result_flows(9) != 777)
        return 11;
    /* 加 0 也必须按位域类型回读：r = 6 */
    if (t_result_flows(0) != 766)
        return 12;
    return 0;
}
