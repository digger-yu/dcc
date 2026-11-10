/*
 * 语法：`for` 初始化子句里的声明带 __attribute__((cleanup)) 时，析构必须落在
 * **整个 for 语句**结束时，而不是初始化子句结束时（报告 §5.36）。
 *
 * 现场：内核 include/linux/cleanup.h 的 __scoped_guard：
 *
 *   for (CLASS(_name, scope)(args);
 *        __guard_ptr(_name)(&scope) || !__is_cond_ptr(_name);
 *        ({ goto _label; }))
 *       if (0) { _label: break; } else <body>
 *
 * 解析器把「初始化子句里的声明语句」合成一个 Stmt::Block，发射器按普通块处理，
 * 于是 begin/end_cleanup_scope 在循环体之前就配对完了：构造器和析构器背靠背
 * 执行，<body> 整个跑在锁外。内核 try_to_wake_up 的 p->pi_lock 就是这样被
 * 「加锁后立刻解锁」的 —— 后面 raw_spin_rq_lock 拿 rq 锁时 IRQ 已经开着，
 * 定时器中断的 sched_tick 再抢同一把 rq 锁 ⇒ queued_spin_lock_slowpath 活锁。
 *
 * 判据：guard 是否真的罩住了 body，以及 break / continue / return / 嵌套循环
 * 四条出口是否都按 C 语义收尾（for-init 声明的作用域跨迭代存活，只在离开
 * for 语句时析构一次）。
 *
 * 失败码（成功返回 0；assert_all_levels_match_gcc 要求两侧退出码相等且为 0）：
 *   1  guard 体内的 depth 不是 1（析构提前发生）
 *   2  嵌套 guard：内层体内 depth 不是 2，或内层退出后外层不再是 1
 *   4  break 出 guard：析构没发生
 *   8  continue：析构没发生（continue 走 for 的 step，step 就是 goto _label）
 *   16 return：返回路径没析构
 *   32 离开 for 语句后仍处于加锁状态
 *   64 guard 体内的普通内层 for 循环把外层 guard 析构掉了
 *   128 同一个 for 里 guard 被重复构造（作用域跨迭代，只能构造一次）
 */

typedef struct {
    int *lock;
    int tag;
} guard_t;

static int depth;
static int ctor_count;

static guard_t guard_ctor(int *l, int tag) {
    guard_t t;
    t.lock = l;
    t.tag = tag;
    depth++;
    ctor_count++;
    return t;
}

static void guard_dtor(guard_t *g) { depth--; }

#define CLASS_GUARD(var)                                                       \
    guard_t var __attribute__((cleanup(guard_dtor))) = guard_ctor

#define __PASTE2(a, b) a##b
#define PASTE2(a, b) __PASTE2(a, b)
#define UNIQUE_ID(p) PASTE2(PASTE2(u_, p), __COUNTER__)

/* 内核 __scoped_guard 的逐字形状（条件恒真，step 就是 goto _label）。 */
#define __scoped_guard(label, args...)                                         \
    for (CLASS_GUARD(scope)(args); ; ({ goto label; }))                        \
        if (0) {                                                               \
            label:                                                             \
            break;                                                             \
        } else

#define scoped_guard(args...) __scoped_guard(UNIQUE_ID(sc), args)

static int g_lockword;

/* 1 + 32：基本形态；析构必须发生在离开 for 之后。 */
static int case_basic(void) {
    int bad = 0;

    scoped_guard(&g_lockword, 7) {
        if (depth != 1) {
            bad |= 1;
        }
    }
    if (depth != 0) {
        bad |= 32;
    }
    return bad;
}

/* 2：嵌套；内层析构后外层仍持有。 */
static int case_nested(void) {
    int bad = 0;

    scoped_guard(&g_lockword, 1) {
        if (depth != 1) {
            bad |= 2;
        }
        scoped_guard(&g_lockword, 2) {
            if (depth != 2) {
                bad |= 2;
            }
        }
        if (depth != 1) {
            bad |= 2;
        }
    }
    if (depth != 0) {
        bad |= 32;
    }
    return bad;
}

/* 4：break 直接出 guard。 */
static int case_break(void) {
    int bad = 0;

    scoped_guard(&g_lockword, 3) {
        if (depth != 1) {
            bad |= 1;
        }
        break;
    }
    if (depth != 0) {
        bad |= 4;
    }
    return bad;
}

/* 8：continue 落到 step（`({ goto _label; })`）再 break，同样要析构。 */
static int case_continue(void) {
    int bad = 0;

    scoped_guard(&g_lockword, 4) {
        if (depth != 1) {
            bad |= 1;
        }
        continue;
    }
    if (depth != 0) {
        bad |= 8;
    }
    return bad;
}

/* 16：return 路径上必须析构。 */
static int case_return_helper(void) {
    scoped_guard(&g_lockword, 5) {
        if (depth != 1) {
            return 1;
        }
        return 0;
    }
    return 2; /* 不可达 */
}

static int case_return(void) {
    int bad = 0;

    if (case_return_helper() != 0) {
        bad |= 16;
    }
    if (depth != 0) {
        bad |= 16;
    }
    return bad;
}

/* 64：guard 体内的普通 for 循环，出口不得把外层 guard 析构掉。 */
static int case_inner_loop(void) {
    int bad = 0;
    int j;

    scoped_guard(&g_lockword, 6) {
        for (j = 0; j < 3; j++) {
            if (depth != 1) {
                bad |= 64;
            }
        }
        if (depth != 1) {
            bad |= 64;
        }
    }
    if (depth != 0) {
        bad |= 32;
    }
    return bad;
}

/* 128：作用域跨迭代 —— 同一个 for 里的 guard 只构造/析构一次。 */
static int case_construct_once(void) {
    int bad = 0;
    int i;

    ctor_count = 0;
    for (i = 0; i < 3; i++) {
        scoped_guard(&g_lockword, i) {
            if (depth != 1) {
                bad |= 1;
            }
        }
    }
    if (ctor_count != 3) {
        bad |= 128;
    }
    return bad;
}

int main(void) {
    int bad = 0;

    bad |= case_basic();
    bad |= case_nested();
    bad |= case_break();
    bad |= case_continue();
    bad |= case_return();
    bad |= case_inner_loop();
    bad |= case_construct_once();
    if (depth != 0) {
        bad |= 32;
    }
    return bad;
}
