/*
 * 报告 §5.24：8 字节聚合变量的初始化必须按「交付轴」判整块拷贝。
 *
 * 内核 include/linux/cleanup.h 的 `guard(mutex)(&m)` 展开形：
 *     class_mutex_t v __cleanup(destructor) = constructor(&m), *aux __cleanup(...) = ...
 * 构造器是 `static __always_inline`，返回 class_mutex_t —— 一个
 * `struct { struct mutex *lock; }`（8 字节，装得进一个寄存器）。内联后
 * 初始化表达式是 StmtExpr，发射端在 rax 里交付的是**物化槽地址**；
 * 旧判据「目标 size > 8 或源是 CompoundLit」把它判成标量交付，
 * `mov_to_stack` 于是把栈地址写进变量（而不是 lock 指针）。
 * destructor 读到的 g->lock 是栈地址，对栈「解锁」，真实 mutex 的
 * owner 永不清零 ⇒ 第二次加锁自旋自死锁（boot 停在
 * `RCU Tasks: Setting shift to 0 …` 之后，报告 §5.24）。
 *
 * 失败码：
 *  1 = cleanup 变量槽里不是 lock 指针（内联构造器 + 块声明）
 *  2 = 真实 mutex 未被解锁
 *  3 = 加锁/解锁计数不对
 *  4 = 函数顶层声明的 8 字节 struct（functions.rs 站点）
 *  5 = 函数顶层声明的 4 字节 struct
 *  6 = 块内声明的 8 字节 struct（stmt.rs 站点）
 *  7 = 语句表达式块内声明的 8 字节 struct（dispatch.rs 站点）
 *  8-11 = 其余聚合拷贝形态（Ident 源拷贝初始化/赋值、Deref 目标、4B 拷贝）
 */
typedef unsigned long u64;

struct mutex {
    long owner;
};
struct guard {
    struct mutex *lock;
}; /* 8B */
struct small {
    int a;
    int b;
}; /* 8B */
struct tiny {
    int a;
}; /* 4B */

static struct mutex g_mu;
static int locks, unlocks;

static inline __attribute__((always_inline)) struct guard
guard_ctor(struct mutex *l)
{
    struct guard t = {.lock = l};
    t.lock->owner = 1;
    locks++;
    return t;
}

static void guard_dtor(struct guard *g)
{
    if (g->lock) {
        g->lock->owner = 0;
        unlocks++;
    }
}

/* 内联后，8 字节聚合初始化的交付形态是 StmtExpr（地址）。 */
int test_guard(void)
{
    {
        struct guard g __attribute__((cleanup(guard_dtor))) = guard_ctor(&g_mu);
        if (g.lock != &g_mu)
            return 1;
    }
    if (g_mu.owner != 0)
        return 2;
    if (locks != 1 || unlocks != 1)
        return 3;
    return 0;
}

static inline __attribute__((always_inline)) struct small
mk_small(int a, int b)
{
    struct small t = {a, b};
    return t;
}

int test_small(void)
{
    struct small s = mk_small(7, 9);
    if (s.a != 7 || s.b != 9)
        return 4;
    return 0;
}

static inline __attribute__((always_inline)) struct tiny
mk_tiny(int a)
{
    struct tiny t = {a};
    return t;
}

int test_tiny(void)
{
    struct tiny t = mk_tiny(5);
    if (t.a != 5)
        return 5;
    return 0;
}

/* stmt.rs 块声明站点：块内局部变量。 */
int test_block(void)
{
    int fail = 0;
    {
        struct small s = mk_small(1, 2);
        if (s.a != 1 || s.b != 2)
            fail = 6;
    }
    return fail;
}

/* dispatch.rs 语句表达式站点：`({ ... })` 块内的声明。 */
int test_stmtexpr(void)
{
    struct small s = ({
        struct small t = mk_small(3, 4);
        t;
    });
    if (s.a != 3 || s.b != 4)
        return 7;
    return 0;
}

/* 其余已知正确的 8/4 字节聚合拷贝形态（回归护栏：它们必须保持正确）。 */
int test_copy_shapes(void)
{
    struct small a = {3, 4};
    struct small b = a; /* Ident 源：8B 拷贝初始化 */
    if (b.a != 3 || b.b != 4)
        return 8;
    struct small c;
    c = a; /* Ident 源：8B 赋值 */
    if (c.a != 3 || c.b != 4)
        return 9;
    struct small d;
    struct small *p = &d;
    *p = (struct small){7, 8}; /* Deref 目标 + 8B 复合字面量 */
    if (d.a != 7 || d.b != 8)
        return 10;
    struct tiny t = {5};
    struct tiny u = t; /* 4B Ident 拷贝 */
    if (u.a != 5)
        return 11;
    return 0;
}

int main(void)
{
    int r;
    if ((r = test_guard()))
        return r;
    if ((r = test_small()))
        return r;
    if ((r = test_tiny()))
        return r;
    if ((r = test_block()))
        return r;
    if ((r = test_stmtexpr()))
        return r;
    if ((r = test_copy_shapes()))
        return r;
    return 0;
}
