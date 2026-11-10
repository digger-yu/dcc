// tests/fixtures/c/attr_cleanup.c
// __attribute__((cleanup(fn))) 的每一个退出点：块结束、函数体 `return`、函数体自然
// 落空、以及「嵌套作用域按 LIFO」与「一个函数多个 return 站点每个都要发」。
//
// 为什么值得把这些形状钉在一个夹具里：内核 `include/linux/cleanup.h` 的
// `guard(raw_spinlock_irqsave)(&ioapic_lock);` 展开成一个**函数体层**的带
// `__cleanup__` 声明符，解锁只可能发生在后两类退出点上。今天之前只有块出口被发射，
// 于是 `io_apic_get_redir_entries` 里只有一条 `_raw_spin_lock_irqsave` 调用、没有任何
// 解锁字节 —— PVH 启动走到 `Processor #0 (Bootup-CPU)` 后静默卡死在
// `queued_spin_lock_slowpath`（报告 §5.16）。
//
// 下面的期望值全部取自 gcc 在同批形状上的实测行为（-O0/-O2 一致）。

static int runs;       // cleanup 被调用的次数
static int last_value; // 最近一次 cleanup 读到的变量值
static int seq[4];     // 每次 cleanup 读到的值（LIFO 序），多出的丢弃
static int nseq;

static void set_zero(int *p) {
    runs++;
    last_value = *p;
    if (nseq < 4) {
        seq[nseq++] = *p;
    }
    *p = 0;
}

static void null_ptr(char **p) {
    runs++;
    last_value = (int)(long)*p;
    *p = (char *)0;
}

static void reset(void) {
    runs = 0;
    last_value = -1;
    nseq = 0;
    seq[0] = -1;
    seq[1] = -1;
}

/* 1. 块作用域 —— 历史唯一被发射的形状 */
static int block_scope(void) {
    reset();
    {
        int x __attribute__((cleanup(set_zero))) = 42;
        if (x != 42) {
            return 1;
        }
    }
    if (runs != 1) {
        return 2;
    }
    if (last_value != 42) {
        return 3;
    }
    return 0;
}

/* 2. 函数体 + `return 表达式`：cleanup 要跑，且已算出的返回值必须活着 */
static int fn_return_preserves_value(void) {
    int x __attribute__((cleanup(set_zero))) = 7;
    reset();
    return x + 35;
}

/* 3. 函数体 + 自然落空（没有 return 语句） */
static void fn_fallthrough(void) {
    int x __attribute__((cleanup(set_zero))) = 11;
    reset();
    (void)x;
}

/* 4. 两个作用域 + 两个 return 站点：早退那条要按 LIFO 发两层，
      函数末尾那条也要发外层 —— 登记栈只有一份，消费式的写法只会
      让后发射的那个站点拿到空栈。 */
static int two_scopes_two_returns(int flag) {
    int outer __attribute__((cleanup(set_zero))) = 1;
    reset();
    if (flag) {
        int inner __attribute__((cleanup(set_zero))) = 2;
        return 5;
    }
    return 6;
}

/* 5. 指针返回值同样要跨过 cleanup 的 `call` */
static char *ptr_return(void) {
    char *s __attribute__((cleanup(null_ptr))) = (char *)"hello";
    reset();
    return s;
}

int main(void) {
    if (block_scope() != 0) {
        return 1;
    }

    int v = fn_return_preserves_value();
    if (v != 42) {
        return 2; // 返回值被 cleanup 的 call 毁掉
    }
    if (runs != 1 || last_value != 7) {
        return 3; // return 路径没执行 cleanup
    }

    fn_fallthrough();
    if (runs != 1 || last_value != 11) {
        return 4; // 函数末尾落空没执行 cleanup
    }

    if (two_scopes_two_returns(1) != 5) {
        return 5;
    }
    if (runs != 2 || seq[0] != 2 || seq[1] != 1) {
        return 6; // LIFO 序错
    }

    if (two_scopes_two_returns(0) != 6) {
        return 7;
    }
    if (runs != 1 || seq[0] != 1) {
        return 8; // 第二个 return 站点没发 cleanup
    }

    char *p = ptr_return();
    if (p[0] != 'h') {
        return 9; // 指针返回值没跨过 cleanup
    }
    if (runs != 1) {
        return 10;
    }
    return 0;
}
