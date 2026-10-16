/* 回归夹具: `&&` / `||` 的右操作数被落到守卫之外 (SSA 通道)。
**
** SSA 通道不为短路求值造块: 整个条件是一棵交给重建的表达式树, 树里每个值都必须内联
** 进树, 否则它就退化成块里一条独立赋值语句, 而独立语句按块序发射 —— 发在带守卫的分支
** **之前**, 于是「被守卫保护的那次求值」提前发生: 轻则读到未初始化的值而走进错误分支,
** 重则直接打空段错误。判据见 `src/ssa/inline_temps.rs::lazy_operand_landed`。
**
** 三个犯错的形状都取自 sqlite 实测: `land_addr` 是 `sqlite3ExprFunction` 里
** `pList && pList->nExpr > db->aLimit[SQLITE_LIMIT_FUNCTION_ARG]` (`count(*)` 传的
** pList 恒为 0); `land_member_chain` 是 `p && p->q->r` 这类两段指针追逐。
** 两个对照形状 (`ctrl_const` / `ctrl_nested`) 本来就不犯 —— 它们用来钉住「判据不要
** 把还能正确重建的函数也一并赶出 SSA」之外的行为 (值必须逐档与 gcc 一致)。 */
typedef struct Limits {
    int aLimit[16];
} Limits;
typedef struct Inner {
    int r;
} Inner;
typedef struct Outer {
    Inner *q;
    int n;
} Outer;
typedef struct List {
    int nExpr;
} List;

static int n_many;
static int n_hit;
static int n_ctl;

__attribute__((noinline)) static int land_addr(Limits *db, List *pl, int nested) {
    if (pl && pl->nExpr > db->aLimit[5] && !nested)
        n_many++;
    return n_many;
}

__attribute__((noinline)) static int land_member_chain(Limits *db, Outer *po, int k) {
    (void)db;
    if (po && po->q->r > k)
        n_hit++;
    return n_hit;
}

__attribute__((noinline)) static int lor_guard(Limits *db, List *pl) {
    if (!pl || pl->nExpr > db->aLimit[5])
        n_hit++;
    return n_hit;
}

__attribute__((noinline)) static int ctrl_const(Limits *db, List *pl) {
    (void)db;
    if (pl && pl->nExpr > 5)
        n_ctl++;
    return n_ctl;
}

__attribute__((noinline)) static int ctrl_nested(Limits *db, List *pl) {
    if (pl) {
        if (pl->nExpr > db->aLimit[5])
            n_ctl++;
    }
    return n_ctl;
}

int main(void) {
    Limits lim;
    List lst;
    Inner in;
    Outer out;
    int i;
    int mask = 0;
    for (i = 0; i < 16; i++)
        lim.aLimit[i] = 128;
    lst.nExpr = 200;
    in.r = 7;
    out.q = &in;

    /* 守卫必须真的挡住右操作数: 三个空指针调用不得有任何读。 */
    if (land_addr(&lim, (List *)0, 0) != 0)
        mask |= 1;
    if (land_member_chain(&lim, (Outer *)0, 3) != 0)
        mask |= 2;
    /* `!pl || ...`: pl 为空时短路成立 ⇒ n_hit 记一次, 但不读 pl->nExpr。 */
    if (lor_guard(&lim, (List *)0) != 1)
        mask |= 4;
    if (ctrl_const(&lim, (List *)0) != 0)
        mask |= 8;
    if (ctrl_nested(&lim, (List *)0) != 0)
        mask |= 16;
    /* 非空时走完整条式子: 值由比较决定。 */
    if (land_addr(&lim, &lst, 0) != 1)
        mask |= 32;
    if (land_addr(&lim, &lst, 1) != 1)
        mask |= 64;
    if (land_member_chain(&lim, &out, 3) != 2)
        mask |= 128;
    if (ctrl_const(&lim, &lst) != 1)
        mask |= 256;
    if (ctrl_nested(&lim, &lst) != 1)
        mask |= 512;
    return mask;
}
