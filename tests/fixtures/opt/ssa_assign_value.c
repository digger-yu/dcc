/* SSA 写侧的「赋值表达式的值」逐档与 gcc 对拍（C11 6.5.16p3）。
 *
 * `lower_lvalue`（`src/ssa/lower.rs`）负责把「以左值形态写内存」降级成一条写指令
 * （`Store` / `IndexStore` / `MemberStore`，`dest` 为空 —— 写本身没有结果值）。
 * 旧实现在发完写之后 `b.fresh("store"|"idxstore"|"memstore", ty)` 造一个新临时变量
 * 当**赋值表达式的值**返回，却没有任何指令写过它。SSA→AST 重建于是为那个变量分配
 * 栈槽而只读不写，-O2/-O3 下读回的是栈垃圾：
 *
 *   `typedef unsigned int u32; u32 ui[8]; unsigned long long vv;`
 *   `long L(void){ ui[0]=0; vv=0x100000005ULL; return (ui[0] = vv); }`
 *   gcc 5，dcc -O2/-O3 给 317169664 之类的垃圾；-O0/-O1（AST 通道）是对的。
 *
 * 命中整类形态：解引用、下标（含多维）、成员（点与箭头）、联合体成员、位域、
 * 窄类型（截断与符号性都要取左值那一侧）、浮点右值转整数左值。
 *
 * 修法：写完紧跟一条**同址同类型**的载入（`Load` / `IndexLoad` / `MemberLoad`），
 * 它的 `dest` 才是赋值结果 —— 那条载入正是 C 语义里的「左值转换」，宽度与符号性
 * 由后端按左值类型发，不在 SSA 层复制布局算术。载入标 `is_volatile: false`：它只是
 * 取回刚写进去的位，不是源码里的一次 volatile 访问，标成 volatile 会让「值没被用」
 * 的赋值也多保留一次读。箭头分支的读用 canonical 形态（`obj` = 指针值 +
 * `is_arrow: true`），不沿用写侧那条把指针按字段类型预读成标量的中间形态 ——
 * 同一个形状在读侧出过 sqlite3BtreeCursor 收垃圾指针段错误的事。
 *
 * 通过: exit 0，且 stdout 与 gcc -O0 参照逐字节相同（-O0/-O1/-O2/-O3 各跑一遍）。
 */

#include <stdio.h>

typedef unsigned int u32;
typedef unsigned long long u64;
typedef signed char i8;
typedef unsigned short u16;

struct S {
    int a;
    int b;
    double d;
    float g;
};
struct BF {
    unsigned x : 3;
    unsigned y : 5;
};
union U {
    int i;
    double f;
};

u32 ug[8];
u64 vvg;
int mm[2][3];
int gi;

static int twice(int v) {
    return v * 2;
}

/* 解引用左值：具名局部（被取址 ⇒ 住内存）与全局 */
int t_deref(void) {
    int l = 0;
    int *p = &l;
    int r1 = (*p = 7);
    int r2 = (*p = l);
    gi = 0;
    int *q = &gi;
    int r3 = (*q = 9);
    return r1 + r2 + r3 + l + gi;
}

/* 下标左值：全局数组 / 局部数组 / 二维（内外层都算地址） */
long t_index(void) {
    ug[0] = 0;
    long r1 = (ug[0] = 7u);
    vvg = 0x100000005ULL;
    long r2 = (ug[0] = vvg); /* 64 位右值截成 u32 = 5 */
    int la[4];
    la[0] = 0;
    long r3 = (la[1] = 7);
    int i = 1, j = 2;
    long r4 = (mm[i][j] = 8);
    return r1 + r2 + r3 + r4 + mm[1][2] + (long)ug[0];
}

/* 成员左值：点与箭头，箭头命中非首字段（偏移由 codegen 在指针值上加） */
int t_member(void) {
    struct S l;
    l.a = 1;
    l.b = 2;
    int r1 = (l.b = 7);
    struct S *p = &l;
    int r2 = (p->b = 5);
    int r3 = (p->a = 3);
    long r4 = (p->d = 2.5); /* 结果类型 = double，取整后为 2 */
    gi = 0;
    return r1 + r2 + r3 + (int)r4 + l.b + l.a + twice(p->b) + (int)p->d;
}

/* 箭头成员的值参与后续计算（写进真对象，不是 `*(fld_ty*)p` 那个 offset 0） */
int t_arrow_flows(void) {
    struct S l;
    l.a = 111;
    l.d = 0.0;
    struct S *p = &l;
    int v = (p->b = 7);
    return v + p->b + l.a + (int)p->d;
}

/* 窄类型：截断方向与符号性都取左值那一侧 */
long t_narrow(void) {
    i8 c;
    i8 *dp = &c;
    long r1 = (*dp = 300); /* signed char: 300 → 44 */
    u16 s;
    u16 *sp = &s;
    long r2 = (*sp = 70000u);
    struct BF b;
    b.x = 0;
    long r3 = (b.x = 300u); /* 3 位: 300 & 7 = 4 */
    struct BF *bp = &b;
    long r4 = (bp->y = 300u); /* 5 位: 300 & 31 = 12 */
    return r1 + r2 + r3 + r4 + b.x + b.y;
}

/* 联合体成员 + 浮点右值转整数左值 */
int t_union_and_conv(void) {
    union U u;
    u.i = 0;
    int r1 = (u.i = 7);
    struct S l;
    l.a = 0;
    int r2 = (l.a = 2.7);
    double gd = 0.0;
    double *gp = &gd;
    int r3 = (int)(*gp = 2.5);
    return r1 + r2 + r3 + u.i + l.a;
}

/* 链式赋值与「值作为实参」：两条都依赖写侧结果值正确 */
int t_chain(void) {
    int a = 0, b = 0;
    int r1 = (a = b = 9);
    struct S l;
    l.b = 1;
    struct S *p = &l;
    int r2 = twice(p->b = 7);
    int r3 = (l.a = twice(p->b = 3));
    return r1 + r2 + r3 + a + b + p->b + l.a;
}

/* 值没被用到的写（语句形态）必须保持原样：多出来的读要能被消掉，写不能丢 */
int t_stmt_only(void) {
    ug[0] = 0;
    ug[3] = 7;
    struct S l;
    struct S *p = &l;
    p->d = 2.5;
    int i = 1, j = 2;
    mm[i][j] = 5;
    return ug[3] + (int)p->d + mm[1][2];
}

/* 成员里的标量浮点读/写：地址寄存器落在 r8..r15 时，`movss/movsd` 的 REX.B 必须
 * 排在 legacy 前缀（F2/F3）**之后** —— 之前这里是两条 `raw_bytes` 反着发，REX 被
 * 作废，装载退化成 `movsd (%rax),%xmm0`，实测 `return r + (int)l.d;` 直接段错误
 * （逐档都崩，因为崩在 AST 通道的公共助手里，不在 SSA 里）。 */
int t_float_member(void) {
    struct S l;
    l.d = 1.0;
    l.g = 2.5f;
    int r = 2;
    struct S *p = &l;
    int a = r + (int)l.d;    /* 点 + double → int */
    p->d = 3.5;              /* 箭头 + double 写 */
    int b = r + (int)p->d;
    int c = r + (int)l.g;    /* float 读 */
    double e = l.d + p->d;   /* 两支都走 xmm */
    int d2 = r + (int)e;
    double dv = 4.0;
    double *dp = &dv;
    *dp = 7.5;               /* 经指针写 double */
    return a + b + c + d2 + (int)(*dp) + (int)p->d;
}

int main(void) {
    printf("deref=%d\n", t_deref());
    printf("index=%ld\n", t_index());
    printf("member=%d\n", t_member());
    printf("arrow=%d\n", t_arrow_flows());
    printf("narrow=%ld\n", t_narrow());
    printf("union=%d\n", t_union_and_conv());
    printf("chain=%d\n", t_chain());
    printf("stmt=%d\n", t_stmt_only());
    printf("float=%d\n", t_float_member());
    return 0;
}
