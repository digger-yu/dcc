/* 嵌套聚合成员赋值的**基址必须是对象地址**（sqlite `yy_shift` 丢写）
 *
 * `lower_lvalue`（`src/ssa/lower.rs`）的成员写分支里，字段是标量时早已改走
 * 「整条左值取址 + `Store`/`Load`」；字段本身是聚合（`o.a.b = v`、`p->u.s = v`、
 * `tos->minor.s = v`）时留在 `MemberStore` 形态上，而那条形态过去的基址是
 * `b.lower_expr(expr)` —— struct/union 基址的**右值**形态在 SSA 层是整块拷贝。
 * 实测 -O2 下 sqlite `yy_shift` 的 `yytos->minor.yy0 = yyMinor`（基址
 * `yytos->minor` 是 union）编成 `rep movsq` 16 字节到 `-0x78(%r13)` 再写字段：
 * 写落进帧临时，`yypParser->yystack` 一动不动，解析器随即拿垃圾 stateno/major 而崩。
 * 同一形状的**箭头**分支（`p->sub = other`）也错：它前置一条
 * `Load{e: p, ty: fld_ty}` 把字段按值读进临时再对临时做 dot 写，写同样丢失
 * （读侧因同一个形状出过事，见 `build.rs` 的 `Expr::Member` 读分支注释）。
 *
 * 修复与读侧共用一套归一化：基址一律经 `lower_lvalue_base_addr` 取**地址**
 * （`MemberAddr`/`IndexAddr`），`member_base_is_ptr_operand` 把 struct/union 类型的
 * `Member`/`Deref`/`Index` 基址都判成指针形态，`is_arrow`/`obj_ty` 跟着操作数形态走，
 * 整块拷贝仍由 codegen 的聚合成员写完成（SSA 层不复制布局算术）。
 *
 * 夹具覆盖的形态：局部/全局/数组元素/三级嵌套 × 具名对象/指针/解引用基址 ×
 * 字段是 struct / union 内 struct / union 本体 × 标量字段与聚合字段两种写 ×
 * 赋值的表达式值（C11 6.5.16p3）× 经联合成员别名读回。
 *
 * 通过: exit 0（每个函数写完立刻按另一条路径读回，期望值与 gcc 各档位一致）
 */

struct Inner {
    int a;
    char b;
};
struct Outer {
    struct Inner in;
    int tail;
};
struct Top {
    struct Outer o;
};
union U {
    int i;
    struct Inner s;
};
struct Entry {
    unsigned short stateno;
    unsigned short major;
    union U minor;
};
struct Parser {
    struct Entry *tos;
    struct Entry *end;
    struct Entry stk[8];
};

static struct Outer go;
static struct Outer ga[3];
static struct Top gtop;
static union U gu;
static struct Parser gp;

/* `lo.in = v`：具名局部对象上的聚合字段整体赋值，再用指针读回。 */
static int nested_local(void)
{
    struct Outer lo;
    struct Inner v;
    struct Outer *p;

    v.a = 1234;
    v.b = 77;
    lo.in = v;
    lo.tail = 3;
    p = &lo;
    if (p->in.a != 1234 || p->in.b != 77 || p->tail != 3)
        return 1;
    p->in.a = 99;
    if (lo.in.a != 99 || lo.in.b != 77)
        return 2;
    return 0;
}

/* sqlite `yy_shift` 的形状：指针链上的 union 成员里再写 struct 字段 / union 本体。 */
static int parser_shift(void)
{
    struct Inner v;
    struct Entry *tos;

    v.a = 555;
    v.b = 6;
    gp.tos = &gp.stk[0];
    gp.end = &gp.stk[7];
    gp.tos++;
    tos = gp.tos;
    tos->stateno = 3;
    tos->major = 4;
    tos->minor.s = v;
    if (tos->minor.s.a != 555 || tos->minor.s.b != 6)
        return 3;
    if (gp.stk[1].minor.s.a != 555 || gp.stk[1].minor.s.b != 6)
        return 4;
    tos->minor.s.b = 9;
    if (gp.stk[1].minor.s.b != 9 || gp.stk[1].minor.s.a != 555)
        return 5;
    return 0;
}

/* union 本体与 union 内标量字段：写一侧、经另一侧读回（别名）。 */
static int union_member_alias(void)
{
    struct Inner v;

    v.a = 1 << 20;
    v.b = 3;
    gu.s = v;
    if (gu.s.a != (1 << 20) || gu.s.b != 3)
        return 6;
    gu.i = 7;
    if (gu.s.a != 7)
        return 7;
    gu.s.b = 5;
    if (gu.i != 7 || gu.s.a != 7 || gu.s.b != 5)
        return 8;
    return 0;
}

/* 全局对象与取址基址混用：`go.in.a` / `(&go)->in.b` / 指针 `gp2->in`。 */
static int global_nested(void)
{
    struct Outer *gp2;
    struct Inner v;

    v.a = 4242;
    v.b = 6;
    go.in = v;
    go.tail = 1;
    gp2 = &go;
    if (gp2->in.a != 4242 || go.in.b != 6)
        return 9;
    gp2->in.b = 11;
    if (go.in.b != 11 || (&go)->in.a != 4242)
        return 10;
    return 0;
}

/* 数组元素上的嵌套成员：写 `ga[i].in`，读回经元素指针与整体拷贝两条路径。 */
static int array_elem_nested(void)
{
    struct Outer *q;

    ga[1].in.a = 4242;
    ga[1].in.b = 6;
    if (ga[1].in.a != 4242 || (&ga[1])->in.b != 6)
        return 11;
    q = &ga[0];
    q->in = ga[1].in;
    if (ga[0].in.a != 4242 || ga[0].in.b != 6)
        return 12;
    q->in.a = 1;
    if (ga[0].in.a != 1 || ga[1].in.a != 4242)
        return 13;
    return 0;
}

/* 三级嵌套 + 解引用基址：`(*pq).o.in.a`、`gtop.o.in.b`。 */
static int three_levels(void)
{
    struct Top *pq;
    struct Inner v;

    v.a = 71;
    v.b = 4;
    pq = &gtop;
    pq->o.in = v;
    if (gtop.o.in.a != 71 || gtop.o.in.b != 4)
        return 14;
    (*pq).o.in.a = 8;
    if (gtop.o.in.a != 8 || gtop.o.in.b != 4)
        return 15;
    gtop.o.in.b = 9;
    if (pq->o.in.b != 9)
        return 16;
    return 0;
}

/* 写侧的表达值（C11 6.5.16p3）：标量嵌套字段 `r = (lo.in.a = 55)`。 */
static int assign_value(void)
{
    struct Outer lo;
    int r;

    lo.in.a = 1;
    lo.tail = 0;
    r = (lo.in.a = 55);
    if (r != 55 || lo.in.a != 55)
        return 17;
    r = (lo.tail += 7);
    if (r != 7 || lo.tail != 7)
        return 18;
    return 0;
}

int main(void)
{
    int rc;

    rc = nested_local();
    if (rc != 0)
        return rc;
    rc = parser_shift();
    if (rc != 0)
        return rc;
    rc = union_member_alias();
    if (rc != 0)
        return rc;
    rc = global_nested();
    if (rc != 0)
        return rc;
    rc = array_elem_nested();
    if (rc != 0)
        return rc;
    rc = three_levels();
    if (rc != 0)
        return rc;
    rc = assign_value();
    if (rc != 0)
        return rc;
    return 0;
}
