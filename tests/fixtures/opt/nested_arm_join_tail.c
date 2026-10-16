/* 回归夹具: SSA→AST 重建在「两条臂各自又是一层分支」的二分上找不到汇聚块，于是把
** If **之后**的公共尾部内联进第一条展开它的臂，兄弟臂因共享 `visited` 拿到空 ——
** 尾部语句连同 `return` 的取值一起丢失，%rax 留着上一条指令的旧值当返回值。
**
** 判据链 (`src/ssa/rebuild.rs` 的 `branch_join`)：臂不是 `Jump` 直链 (自己以 `Branch`
** 终结) ⇒ `walk_to_join` 给不出候选；臂里还有早退的 `Ret` ⇒ `from` 没有真实后支配者，
** ipdom 只剩虚拟 EXIT 占位名被上层过滤成 `None`。三条判据全落空 ⇒ `join=None`，
** 两条臂自由展开。修复见 `arm_join` / `side_only_exits`。
**
** 形状取自 sqlite3.c 实测：`sqlite3SrcListAppend()` 就是
** `if(pList==0){pNew=xMalloc; if(!pNew) return 0; …}else{pNew=enlarge; if(!pNew) return 0; …}`
** 后面接公共尾部 `pItem=&pList->a[pList->nSrc-1]; …; return pList;`。-O2 下 else 臂
** 丢掉尾部并把 nSrc 当指针返回，`selectExpander` 随后读空 SrcList ⇒ 任何两表 JOIN
** 段错误。`err` 出参用来钉住「内层早退那一条臂的语句也不能被顺手搬走」。 */

typedef struct Item {
    int idx;
    int db;
} Item;

typedef struct List {
    int nSrc;
    Item a[4];
} List;

static List storage;

/* 独立参考实现: 与被测函数语义相同, 但把公共尾部在两条臂里各写一遍 —— 没有跨臂汇聚
** 结构, 于是不走 SSA 重建的那条判据, 只提供期望读数。 */
static List *refAppend(List *p, int idx, int db, int *err)
{
    if (p == 0) {
        p = &storage;
        if (!p) { *err = 7; return 0; }
        p->nSrc = 1;
        p->a[0].idx = idx;
        p->a[0].db = db;
        return p;
    }
    if (p->nSrc >= 4) {
        *err = 3;
        return p;
    }
    p->nSrc = p->nSrc + 1;
    p->a[p->nSrc - 1].idx = idx;
    p->a[p->nSrc - 1].db = db;
    return p;
}

/* 被测: 两条臂都是「内层还有一层分支 + 可早退」, 之后共享一段尾部语句。 */
static List *append(List *p, int idx, int db, int *err)
{
    if (p == 0) {
        p = &storage;
        if (!p) { *err = 7; return 0; }
        p->nSrc = 1;
    } else {
        if (p->nSrc >= 4) { *err = 3; return p; }
        p->nSrc = p->nSrc + 1;
    }
    p->a[p->nSrc - 1].idx = idx;
    p->a[p->nSrc - 1].db = db;
    return p;
}

static void digest(const char *tag, List *p, int err)
{
    int i;
    printf("%s n=%d err=%d [", tag, p ? p->nSrc : -1, err);
    for (i = 0; i < 4; i++) {
        printf("%d:%d/%d%c", i, storage.a[i].idx, storage.a[i].db,
               i == 3 ? ']' : ',');
    }
    printf(" p=%s\n", p == &storage ? "storage" : "other");
}

int main(void)
{
    List *p = 0;
    int err = 0;

    /* 第一次: 走 then 臂 (分配 + 尾部落事实)。 */
    p = append(p, 10, 1, &err);
    digest("first", p, err);

    /* 第二、三次: 走 else 臂, 尾部必须照常执行。 */
    p = append(p, 20, 2, &err);
    digest("second", p, err);
    p = append(p, 30, 3, &err);
    digest("third", p, err);

    /* 第四次填满, 第五次触发 else 臂的早退: 不得改写 nSrc, 也要返回同一个 p。 */
    p = append(p, 40, 4, &err);
    digest("fourth", p, err);
    p = append(p, 50, 5, &err);
    digest("fifth", p, err);

    /* 参考实现独立复算同一串调用, 两边读数必须一致。 */
    {
        List *q = 0;
        int qerr = 0;
        int i;
        storage.nSrc = 0;
        for (i = 0; i < 4; i++) { storage.a[i].idx = 0; storage.a[i].db = 0; }
        q = refAppend(q, 10, 1, &qerr);
        digest("ref-first", q, qerr);
        q = refAppend(q, 20, 2, &qerr);
        digest("ref-second", q, qerr);
        q = refAppend(q, 30, 3, &qerr);
        digest("ref-third", q, qerr);
        q = refAppend(q, 40, 4, &qerr);
        digest("ref-fourth", q, qerr);
        q = refAppend(q, 50, 5, &qerr);
        digest("ref-fifth", q, qerr);
        printf("ref-same-pointer %d\n", q == &storage);
    }
    return 0;
}
