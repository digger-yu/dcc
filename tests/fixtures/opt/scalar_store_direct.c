/* 标量写直生（P13 阶段2）的行为夹具：地址先进 rcx、右值再进 rax 的那条臂，
 * 与仍然保留押栈形态的回退臂，必须在四档下都与 gcc 一致。
 *
 * 白名单（右值是「只会写 rax」的形态：字面量 / 名字 / 由 Cast·Member·Deref 套出来的
 * 标量读链）：`p->a = 1`、`p->b = x`、`t->s.arr[i] = x`、`*p = 123`、`s.c = 65`、
 * 全局成员与全局数组、volatile 标量写、`(int)` 强转右值。
 * 名单外（必须仍走今天这条押栈/其它臂，逐字节不许漂）：右值是 Binop/Unary/Call/
 * 下标取址、复合赋值与 ++/--、浮点值通道、16 字节整型、聚合整块拷贝、位域 RMW。
 *
 * 通过: exit 0（末行 `scalar_store_direct: OK`），四档与 gcc 一致。
 */

#include <stdio.h>

struct S {
    int a;
    long b;
    char c;
    unsigned d;
    long arr[3];
};

struct T {
    struct S s;
    long tail[2];
};

struct B {
    unsigned int lo : 3;
    unsigned int hi : 29;
};

static struct S gs;
static int gi[6];
static volatile long vg;

static int fails;

#define EXPECT(expr, want, name)                                                                 \
    do {                                                                                         \
        long got_ = (long)(expr);                                                                \
        if (got_ != (long)(want)) {                                                              \
            fails++;                                                                             \
            printf("FAIL %s: got %ld want %ld\n", (name), got_, (long)(want));                   \
        }                                                                                        \
    } while (0)

/* --- 白名单臂 --- */
static void w_member_const(struct S *p) { p->a = 1; }
static void w_member_name(struct S *p, long x) { p->b = x; }
static void w_nested_index(struct T *t, int i, int x) { t->s.arr[i] = x; }
static void w_char_field(struct S *p) { p->c = 65; }
static void w_global_member(void) { gs.a = 7; }
static void w_global_arr(void) { gi[2] = 9; }
static void w_deref_const(int *p) { *p = 123; }
static void w_deref_name(int *p, int x) { *p = x; }
static void w_volatile(void) { vg = 5; }
static void w_cast_rhs(struct S *p, short x) { p->a = (int)x; }
static void w_deref_chain(struct T *t, long x) { t->s.b = x; }
static void w_read_chain(struct S *p, const struct S *q) { p->d = q->a; }

/* --- 名单外（回退臂必须照原样工作）--- */
static int src_val(void) { return 3; }
static void w_binop_rhs(struct S *p, int x) { p->a = x + 1; }
static void w_unary_rhs(struct S *p, int x) { p->a = -x; }
static void w_call_rhs(struct S *p) { p->a = src_val(); }
static void w_index_rhs(struct S *p, int i) { p->a = (int)p->arr[i]; }
static void w_addeq(int *a, int i, int b) { a[i] += b; }
static void w_inc(int *p) { (*p)++; }
static void w_float_store(double *p, double v) { *p = v; }
static void w_i128(__int128 *p, long v) { *p = (__int128)v << 40; }
static void w_agg(struct S *dst, const struct S *src) { *dst = *src; }
static void w_bitfield(struct B *p, unsigned v) { p->lo = v; }

int main(void) {
    struct S s;
    struct S other;
    struct T t;
    struct B b;
    int buf[4] = { 0, 0, 0, 0 };
    double dd = 0.0;
    __int128 q = 0;
    int i;

    for (i = 0; i < 3; i++) {
        s.arr[i] = 0;
        other.arr[i] = 100 + i;
    }
    s.a = 0; s.b = 0; s.c = 0; s.d = 0;
    other.a = 11; other.b = 22; other.c = 33; other.d = 44;
    t.s.a = 0; t.s.b = 0; t.s.c = 0; t.s.d = 0;
    t.s.arr[0] = t.s.arr[1] = t.s.arr[2] = 0;
    t.tail[0] = t.tail[1] = 0;
    b.lo = 0; b.hi = 0;

    /* 白名单 */
    w_member_const(&s);
    EXPECT(s.a, 1, "member_const");
    w_member_name(&s, -5);
    EXPECT(s.b, -5, "member_name");
    w_nested_index(&t, 1, 77);
    EXPECT(t.s.arr[1], 77, "nested_index_store");
    w_char_field(&s);
    EXPECT((int)s.c, 65, "char_field");
    w_global_member();
    EXPECT(gs.a, 7, "global_member");
    w_global_arr();
    EXPECT(gi[2], 9, "global_array_index");
    w_deref_const(buf);
    EXPECT(buf[0], 123, "deref_const");
    w_deref_name(buf, 999);
    EXPECT(buf[0], 999, "deref_name");
    w_volatile();
    EXPECT(vg, 5, "volatile_store");
    w_cast_rhs(&s, (short)-3);
    EXPECT(s.a, -3, "cast_rhs");
    w_deref_chain(&t, 4242);
    EXPECT(t.s.b, 4242, "deref_chain_store");
    w_read_chain(&s, &other);
    EXPECT((long)s.d, 11, "read_chain_rhs");

    /* 名单外 */
    w_binop_rhs(&s, 10);
    EXPECT(s.a, 11, "binop_rhs");
    w_unary_rhs(&s, 8);
    EXPECT(s.a, -8, "unary_rhs");
    w_call_rhs(&s);
    EXPECT(s.a, 3, "call_rhs");
    s.arr[2] = 55;
    w_index_rhs(&s, 2);
    EXPECT(s.a, 55, "index_rhs");
    w_addeq(buf, 1, 7);
    EXPECT(buf[1], 7, "addeq");
    w_inc(&buf[1]);
    EXPECT(buf[1], 8, "pre_inc_store");
    w_float_store(&dd, 2.5);
    EXPECT(dd > 2.4 && dd < 2.6, 1, "float_store");
    w_i128(&q, 5);
    EXPECT((long)(q >> 40), 5, "i128_store");
    w_agg(&other, &s);
    EXPECT(other.a, 55, "agg_copy_a");
    EXPECT(other.b, -5, "agg_copy_b");
    EXPECT((int)other.c, 65, "agg_copy_c");
    w_bitfield(&b, 12u);
    EXPECT((long)b.lo, 4, "bitfield_trunc");

    if (fails == 0) printf("scalar_store_direct: OK\n");
    return fails;
}
