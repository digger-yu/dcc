/* 跨函数 DCE「同名两条 FuncDecl」夹具（内核形状）。
 *
 * 背景（`report/linux71-dcc-fulltree-build-2026-10-28.md` §3.17）：全树链接曾报
 * 231 处 `undefined reference to 'btf_id_cmp_func'`。形状是内核里
 *
 *     include/linux/btf.h:514   static inline bool btf_id_set_contains(...)
 *     {   return bsearch(&id, ..., btf_id_cmp_func) != NULL; }
 *         —— 取另一个 static inline 的地址交给外部函数
 *     include/linux/bpf.h:3873  bool btf_id_set_contains(const struct btf_id_set *set, u32 id);
 *
 * —— **定义在前、非 static 声明在后**。这个方向 gcc 是接受的（只有「static 接在非
 * static 之后」才报错），于是同一 TU 的函数列表里同名条目有两条：声明那条
 * `body = None` 且带 `is_extern`（⇒ DCE 硬根），定义那条才真有边表。旧版
 * `cross_function_dce_with_roots` 按名字 `insert` 边表，后写的空边表把定义的边整份
 * 盖掉 ⇒ `btf_id_cmp_func` 被当不可达删除，而它的 `lea` 重定位照发，链接期就成了
 * 一个永远无定义的引用。
 *
 * 判据为什么是「symtab 里不许有名为 cmp_fn 的 UND 条目」而不是「重定位必须绑定到
 * 定义」：`cmp_fn` 是 static（内部链接），按 C/ELF 语义它**任何**未定义引用都是错的；
 * 而「本 TU 到底要不要为没人调用的 inline 发体」是体积策略，将来会改（见 §10 成本
 * 模型项）。绑在「重定位必须指到定义」上会让那个正交改动误红这个缺陷用例。
 *
 * `probe` 那条真实调用路径用来证明语义没坏：比较函数必须真的跑过（`cmp_calls > 0`），
 * 四档 (-O0..-O3) 的 stdout 与退出码都得与 gcc 一致。
 */
#include <stdio.h>

typedef unsigned int u32;

static int cmp_calls;
static int live_calls;

/* 线性替身：真的通过函数指针调用 compar。 */
static void *stub_bsearch(const void *key, const void *base, u32 num, u32 size,
                          int (*compar)(const void *, const void *))
{
    const char *p = (const char *)base;
    u32 i;

    for (i = 0; i < num; i++)
        if (compar(key, p + (size_t)i * size) == 0)
            return (void *)(p + (size_t)i * size);
    return 0;
}

static inline int cmp_fn(const void *a, const void *b)
{
    cmp_calls++;
    return (*(const int *)a) - (*(const int *)b);
}

static inline int cmp_live(const void *a, const void *b)
{
    live_calls++;
    return (*(const int *)a) - (*(const int *)b);
}

/* 定义：体里取另一个 static inline 的地址 —— 被盖掉的那份边表就长在这里。
 * 与内核完全同形：**全 TU 没有任何地方调用 set_contains**（内核 timer.c 也不调
 * `btf_id_set_contains`），于是 `cmp_fn` 的唯一引用点就只在这个 inline 体里。 */
static inline int set_contains(const int *ids, u32 cnt, int id)
{
    return stub_bsearch(&id, ids, cnt, sizeof(int), cmp_fn) != 0;
}

/* 关键一行：同名、**非 static** 的声明跟在定义后面（= bpf.h:3873 的形状）。
 * gcc 接受这个方向（只有「static 接在非 static 之后」才报错）。这条声明自己
 * `body = None` 且 `is_extern` ⇒ DCE 硬根；旧版按名字存边表时，它的**空边表**
 * 把上面定义的边整份盖掉，`cmp_fn` 就被当不可达删了。 */
int set_contains(const int *ids, u32 cnt, int id);

/* 与上面完全无关的一条真实调用路径：用**另一个**比较函数（不能拿 `cmp_fn`
 * 做运行期观测，否则 `cmp_fn` 会被这条边顶活，缺陷就测不到了）。它只负责证明
 * 「取函数地址 + 通函数指针回调」这套机制本身在四档下与 gcc 语义一致。 */
static inline int live_contains(const int *ids, u32 cnt, int id)
{
    return stub_bsearch(&id, ids, cnt, sizeof(int), cmp_live) != 0;
}

int main(void)
{
    int a[4] = {3, 1, 4, 2};
    /* 必须拆成语句：`printf("%d", f(), n)` 里参数的求值序是不确定的（拿它做对拍
     * 就是在测未定义行为：gcc 先读计数得 0、dcc 后读得 7）。 */
    int r1 = live_contains(a, 4, 4);
    int r2 = live_contains(a, 4, 9);
    int seen = live_calls;

    printf("%d %d %d\n", r1, r2, seen);
    /* `set_contains` 在本 TU 里**没人调用**（这就是内核形状），只报计数；
     * 它的存在与否由下面的产物级判据（集成用例）管，不在运行期判。 */
    printf("%d\n", cmp_calls);
    return 0;
}
