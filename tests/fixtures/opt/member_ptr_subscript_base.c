/* 指针字段的下标基址必须是**字段值**，不是字段地址（sqlite `sqlite3_str_append`）
 *
 * `Builder::lower_lvalue_base_addr`（`src/ssa/build.rs`）给 `IndexAddr` 的 `arr` 槽
 * 和 `MemberAddr` 的 `obj` 槽求「左值基址」。它把 `Member` 基址一律委托给
 * `lower_member_addr` 时，只在**字段本身是对象**（struct/union/数组）时才对：
 * 那种形态的右值是整块拷贝，必须换成地址（见 `agg_member_store_base.c`）。
 * 字段是**标量指针**时下标基址要的是指针**内容** —— 交回字段地址后
 * `&p->zText[k]` 变成「结构体地址 + 字段偏移 + k」，实测 -O2 下
 * `memcpy(&p->zText[p->nChar-N], z, N)` 把整条字符串写进 `StrAccum` 自己身上，
 * `zText` 指针被覆盖，随后 `sqlite3StrAccumFinish` 解引用垃圾而段错误
 * （`sqlite3_mprintf` 全部命中，sqlite 启动读 sqliterc 即崩）。
 *
 * 夹具覆盖的形态：箭头/点/嵌套成员 × 下标读/下标写/取地址交给 memcpy ×
 * 指针字段数组（两级下标）× 偏移为常量与变量 × 基址按值传参。
 *
 * 通过: exit 0（每个函数按另一条路径读回，期望值与 gcc 各档位一致）
 */

struct Str {
    char *zText;
    int nChar;
    int nAlloc;
};

struct Wrap {
    struct Str s;
    int tail;
};

struct Rows {
    char *rows[4];
    int nrow;
};

#define NOIN __attribute__((noinline))

static char buf[64];

/* `memcpy(&p->zText[p->nChar-N], z, N)` —— sqlite3_str_append 的原形状。 */
static int app_memmove(struct Str *p, const char *z, int N)
{
    int bad = 0;
    p->nChar += N;
    __builtin_memcpy(&p->zText[p->nChar - N], z, N);
    if (buf[0] != 'x' || buf[1] != 'y' || buf[2] != 'z')
        bad |= 1;
    if (p->zText != buf)
        bad |= 2;
    if (p->nChar != N)
        bad |= 4;
    return bad;
}

/* 同一形状、常量偏移、点形态基址。 */
static int app_const(struct Str s)
{
    int bad = 0;
    __builtin_memcpy(&s.zText[0], "abc", 3);
    if (buf[0] != 'a' || buf[2] != 'c')
        bad |= 8;
    return bad;
}

/* 只取地址：`&p->zText[k]` 必须等于 `p->zText + k`。 */
static int addr_value(struct Str *p, int k)
{
    int bad = 0;
    char *a = &p->zText[k];
    if (a != buf + k)
        bad |= 16;
    a = &p->zText[0];
    if (a != buf)
        bad |= 32;
    return bad;
}

/* 下标写：`p->zText[k] = c`。 */
static int store_elem(struct Str *p, int k, char c)
{
    int bad = 0;
    p->zText[k] = c;
    if (buf[k] != c)
        bad |= 64;
    if (p->zText != buf)
        bad |= 128;
    return bad;
}

/* 嵌套成员基址：`w->s.zText[k]`。 */
static int nested_base(struct Wrap *w, int k, char c)
{
    int bad = 0;
    w->s.zText[k] = c;
    if (buf[k] != c)
        bad |= 256;
    if (&w->s.zText[k + 1] != buf + k + 1)
        bad |= 512;
    if (w->s.zText != buf)
        bad |= 1024;
    return bad;
}

/* 指针字段**数组**的两级下标：`r->rows[i][j]`。 */
static int two_level(struct Rows *r, int i, int j, char c)
{
    int bad = 0;
    r->rows[i][j] = c;
    if (buf[j] != c)
        bad |= 2048;
    if (r->rows[i] != buf)
        bad |= 4096;
    if (&r->rows[i][j] != buf + j)
        bad |= 8192;
    return bad;
}

/* 先取字段到局部再下标（这条一直是对的，留作对照）。 */
static int via_local(struct Str *p, int k, char c)
{
    int bad = 0;
    char *t = p->zText;
    t[k] = c;
    if (buf[k] != c)
        bad |= 16384;
    if (p->zText != buf)
        bad |= 32768;
    return bad;
}

int main(void)
{
    struct Str st;
    struct Wrap w;
    struct Rows r;
    int bad = 0;
    int i;

    for (i = 0; i < 64; i++)
        buf[i] = 0;
    st.zText = buf;
    st.nChar = 0;
    st.nAlloc = 64;
    bad |= app_memmove(&st, "xyz", 3);

    buf[0] = 0;
    st.zText = buf;
    bad |= app_const(st);

    buf[5] = 0;
    st.zText = buf;
    bad |= addr_value(&st, 5);

    st.zText = buf;
    bad |= store_elem(&st, 7, 'Q');

    w.s.zText = buf;
    w.s.nChar = 0;
    w.tail = 0;
    buf[3] = 0;
    bad |= nested_base(&w, 3, 'W');

    r.rows[0] = buf;
    r.rows[1] = buf;
    r.nrow = 1;
    buf[2] = 0;
    bad |= two_level(&r, 1, 2, 'T');

    st.zText = buf;
    buf[9] = 0;
    bad |= via_local(&st, 9, 'L');

    return bad;
}
