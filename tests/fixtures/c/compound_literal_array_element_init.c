/*
 * 数组元素是复合字面量 `(T){…}` 时的初始化布局（报告 §5.34）。
 *
 * 现场：内核 `security/security.c` 的 `static_calls_table`：
 *
 *   .capable = {
 *       (struct lsm_static_call){ .key = &__SCK__…, .trampoline = &__SCT__…,
 *                                 .active = &security_hook_active_… },
 *       (struct lsm_static_call){ … },      // MAX_LSM_COUNT = 2
 *   },
 *
 * `Expr::CompoundLit` 自带花括号，是**整个元素**的初始化器；旧实现的 brace
 * elision 判据只认 `Expr::InitList`，把它当扁平标量并按「元素字段数」分组，
 * 于是两个元素被合并进一个：`capable[0].trampoline` 槽拿到 `capable[1].key`、
 * `capable[0].hl` 拿到 `capable[1].trampoline`（非 NULL ⇒ `security_add_hooks`
 * 跳过该槽），`capable[1].key` 拿到 static_key 地址 —— boot 在
 * `__static_call_update` 的 `mod->static_call_sites` 上 double fault。
 *
 * 失败码（成功返回 0；`assert_all_levels_match_gcc` 要求两侧退出码相等且为 0）：
 *   1  静态表 capable[0] 的 key/trampoline/hl/active
 *   2  静态表 capable[1]
 *   3  静态表 settime[0]
 *   4  静态表 settime[1]
 *   5  stride / 总大小（元素被撑大时 offsetof/sizeof 先变形）
 *   6  局部数组（运行时初始化路径）arr[0]
 *   7  局部数组 arr[1]
 *   8  回归：扁平标量仍按 brace elision 分组（struct pt pts[2] = {1,2,3,4}）
 *   9  回归：二维 char 数组按行消费字符串（char w[][6] = {"hello","world"}）
 */

struct static_call_key {
    void *func;
    unsigned long type;
};
struct security_hook_list;
struct static_key_false;

struct lsm_static_call {
    struct static_call_key *key;
    void *trampoline;
    struct security_hook_list *hl;
    struct static_key_false *active;
};

struct lsm_static_calls_table {
    struct lsm_static_call capable[2];
    struct lsm_static_call settime[2];
};

extern struct static_call_key k0, k1, k2, k3;
extern void t0(void), t1(void), t2(void), t3(void);
extern int a0, a1, a2, a3;

struct static_call_key k0, k1, k2, k3;
void t0(void) {}
void t1(void) {}
void t2(void) {}
void t3(void) {}
int a0, a1, a2, a3;

static struct lsm_static_calls_table static_calls_table = {
    .capable = {
        (struct lsm_static_call){
            .key = &k0,
            .trampoline = &t0,
            .active = (struct static_key_false *)&a0,
        },
        (struct lsm_static_call){
            .key = &k1,
            .trampoline = &t1,
            .active = (struct static_key_false *)&a1,
        },
    },
    .settime = {
        (struct lsm_static_call){
            .key = &k2,
            .trampoline = &t2,
            .active = (struct static_key_false *)&a2,
        },
        (struct lsm_static_call){
            .key = &k3,
            .trampoline = &t3,
            .active = (struct static_key_false *)&a3,
        },
    },
};

/* 回归 8：扁平标量列的 brace elision 不能被「自带花括号」判据一并关掉。 */
struct pt {
    int x, y;
};
static struct pt pts[2] = { 1, 2, 3, 4 };

/* 回归 9：二维 char 数组按行消费字符串字面量。 */
static char w[][6] = { "hello", "world" };

static int check_slot(const struct lsm_static_call *s, struct static_call_key *k,
                      void *t, void *a) {
    if (s->key != k) {
        return 1;
    }
    if (s->trampoline != t) {
        return 1;
    }
    if (s->hl != 0) {
        return 1;
    }
    if (s->active != (struct static_key_false *)a) {
        return 1;
    }
    return 0;
}

int main(void) {
    int bad = 0;

    if (check_slot(&static_calls_table.capable[0], &k0, (void *)&t0, &a0)) {
        bad |= 1;
    }
    if (check_slot(&static_calls_table.capable[1], &k1, (void *)&t1, &a1)) {
        bad |= 2;
    }
    if (check_slot(&static_calls_table.settime[0], &k2, (void *)&t2, &a2)) {
        bad |= 4;
    }
    if (check_slot(&static_calls_table.settime[1], &k3, (void *)&t3, &a3)) {
        bad |= 8;
    }
    if (__builtin_offsetof(struct lsm_static_calls_table, settime) != 64
        || sizeof(struct lsm_static_calls_table) != 128) {
        bad |= 16;
    }

    /* 运行时初始化路径（局部数组）：同形状必须同结果。 */
    struct lsm_static_call loc[2] = {
        (struct lsm_static_call){ .key = &k0, .trampoline = &t0,
                                  .active = (struct static_key_false *)&a0 },
        (struct lsm_static_call){ .key = &k1, .trampoline = &t1,
                                  .active = (struct static_key_false *)&a1 },
    };
    if (check_slot(&loc[0], &k0, (void *)&t0, &a0)) {
        bad |= 32;
    }
    if (check_slot(&loc[1], &k1, (void *)&t1, &a1)) {
        bad |= 64;
    }

    if (pts[0].x != 1 || pts[0].y != 2 || pts[1].x != 3 || pts[1].y != 4) {
        bad |= 128;
    }
    if (w[0][0] != 'h' || w[0][4] != 'o' || w[1][0] != 'w' || w[1][4] != 'd') {
        bad |= 256;
    }
    return bad;
}
