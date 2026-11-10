/*
 * 语法：结构体成员 / 数组元素被「**8 字节聚合**语句表达式」初始化（报告 §5.35）。
 *
 * 现场：内核 `mm/mempolicy.c` 的 `numa_policy_init()`：
 *
 *   preferred_node_policy[nid] = (struct mempolicy) {
 *       .refcnt = ATOMIC_INIT(1),
 *       .mode   = MPOL_PREFERRED,
 *       .flags  = MPOL_F_MOF | MPOL_F_MORON,
 *       .nodes  = nodemask_of_node(nid),    // 语句表达式，1 个 unsigned long
 *   };
 *
 * `nodemask_of_node(n)` 是 `({ nodemask_t m; m.b[0] = 1UL << n; m; })`：类型是
 * 8 字节结构体，`gen_stmt_expr_value` 把结果整块拷进 sret 物化槽并交付**槽地址**。
 * 成员初始化路径的经验判据只有「尺寸 > 8 或 CompoundLit」，8 字节语句表达式两条
 * 都不命中，于是 `gen_expr + emit_store(8)` 把物化槽地址当成 `nodes` 的值存下：
 * `first_node(pol->nodes)` 从栈地址字节里取位号得到 3，`node_data[3]` 为 NULL，
 * `ac->zonelist = NULL + 0x1500`，`prepare_alloc_pages` 在 0x1508 上 #PF → #DF。
 *
 * 修复：成员/元素赋值一律问 `assign_needs_whole_copy`（类型轴 + 交付轴单一真源），
 * 交付轴成立时按对象大小整块搬运，与对象是不是 8 字节无关。
 *
 * 失败码（成功返回 0；assert_all_levels_match_gcc 要求两侧退出码相等且为 0）：
 *   1  静态数组元素：compound literal 的 .v.nodes（nid=0）与其余成员
 *   2  静态数组元素：nid=1
 *   4  直接成员赋值 p.v.nodes = ({…})
 *   8  局部数组元素初始化 nodemask_t arr[2] = { ({…}), ({…}) }
 *   16 回归：8 字节成员的普通花括号初始化 { .v.nodes = { 4UL } } 不得被整块口径带偏
 *   32 回归：值在寄存器里交付的 8 字节 Call 返回值（不得按「地址」处理）
 *   64 回归：> 8 字节成员/元素走语句表达式仍要整块拷贝
 *   128 回归：Ident 源拷贝
 *   256 回归：标量成员照旧按标量写
 */

typedef struct {
    unsigned long b[1];
} nodemask_t;

#define nodemask_of_node(node)                                                 \
    ({                                                                         \
        nodemask_t __m;                                                        \
        __m.b[0] = 1UL << (node);                                              \
        __m;                                                                   \
    })

struct mempolicy {
    int refcnt;
    unsigned short mode;
    unsigned short flags;
    int home_node;
    union {
        nodemask_t nodes;
        int preferred_node;
    } v;
};

struct big {
    unsigned long a, b;
};

#define big_of(v)                                                              \
    ({                                                                         \
        struct big __b;                                                        \
        __b.a = (v);                                                           \
        __b.b = (v) + 1;                                                       \
        __b;                                                                   \
    })

struct holder {
    int tag;
    struct big b;
    unsigned long tail;
};

static struct mempolicy preferred_node_policy[4];
static struct mempolicy direct_policy;
static struct holder big_holder;
static int scalar_member;

/* 寄存器交付的 8 字节返回值：它的 rax 里是**数据**而不是缓冲地址。 */
static nodemask_t make_mask(int n) {
    nodemask_t m;
    m.b[0] = 1UL << n;
    return m;
}

static void numa_policy_init(void) {
    int nid;
    for (nid = 0; nid < 2; nid++) {
        preferred_node_policy[nid] = (struct mempolicy){
            .refcnt = 1,
            .mode = 1,
            .flags = 0x18,
            .v.nodes = nodemask_of_node(nid),
        };
    }
}

static void direct_member_init(void) {
    nodemask_t x, reg;
    nodemask_t arr[2] = { nodemask_of_node(0), nodemask_of_node(1) };
    struct mempolicy braced = { .refcnt = 3, .mode = 4, .flags = 5,
                                .v.nodes = { 4UL } };
    struct holder h = { .tag = 9, .b = big_of(11), .tail = 12 };
    int bad = 0;

    if (arr[0].b[0] != 1UL || arr[1].b[0] != 2UL) {
        bad |= 8;
    }
    if (braced.v.nodes.b[0] != 4UL || braced.refcnt != 3 || braced.mode != 4
        || braced.flags != 5) {
        bad |= 16;
    }
    reg = make_mask(5);
    if (reg.b[0] != (1UL << 5)) {
        bad |= 32;
    }
    if (h.b.a != 11UL || h.b.b != 12UL || h.tag != 9 || h.tail != 12) {
        bad |= 64;
    }
    x = nodemask_of_node(2);
    direct_policy.v.nodes = x;
    direct_policy.v.nodes = nodemask_of_node(3);
    if (direct_policy.v.nodes.b[0] != (1UL << 3)) {
        bad |= 4;
    }
    if (x.b[0] != (1UL << 2)) {
        bad |= 128;
    }
    direct_policy.refcnt = 77;
    if (direct_policy.refcnt != 77) {
        bad |= 256;
    }
    big_holder.b = big_of(7);
    if (big_holder.b.a != 7UL || big_holder.b.b != 8UL) {
        bad |= 64;
    }
    scalar_member = bad;
}

int main(void) {
    int bad = 0;
    nodemask_t five;

    numa_policy_init();
    if (preferred_node_policy[0].v.nodes.b[0] != 1UL
        || preferred_node_policy[0].refcnt != 1
        || preferred_node_policy[0].mode != 1
        || preferred_node_policy[0].flags != 0x18) {
        bad |= 1;
    }
    if (preferred_node_policy[1].v.nodes.b[0] != 2UL) {
        bad |= 2;
    }

    direct_member_init();
    bad |= scalar_member;

    /* 调用没有带来副作用，这里再查一次 Call 的值模型（-O2 下内联也不得变）。 */
    five = make_mask(6);
    if (five.b[0] != (1UL << 6)) {
        bad |= 32;
    }
    return bad;
}
