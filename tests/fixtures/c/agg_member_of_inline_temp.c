/* 报告 §5.52：`__always_inline` 形参临时量（内联展开后的 StmtExpr）在 rax 里
 * 交付的是**对象地址**，不是结构体数据。成员访问的物化门旧写法只排除了
 * `CompoundLit`，于是把语句表达式交付的地址当值再存一遍 —— 内核
 * `vma_flags_to_legacy(append_vma_flags(…)).__vma_flags[0]` 读回的因此是
 * 栈指针（`0xffffc900…`），VM_STACK_FLAGS 算成垃圾, GUP 缺 VM_WRITE
 * 返回 -EFAULT，`kernel_execve("/init")` 映射成 -E2BIG。
 *
 * 判据与单一真源 `rvalue_is_buffer_addr` 一致：语句表达式/内存返回调用/
 * 聚合三元/链式赋值都是**地址交付**。夹具用 1/2/4 逐位标记出错的那条路径：
 *   1 单层成员（8 B 结构）        2 16 B 结构成员        3 链式（.a.b）
 *   4 数组字段首元素              5 复合字面量对照       6 普通调用对照
 */
typedef unsigned long u64;

/* 内核 vma_flags 链的最小复刻：bitmap 数组成员。 */
typedef struct {
    u64 w[1];
} flags_t;

static __attribute__((always_inline)) flags_t mk_flags(u64 v) {
    flags_t f = (flags_t){};
    f.w[0] = v;
    return f;
}

static __attribute__((always_inline)) u64 low_word(flags_t f) {
    return f.w[0];
}

struct pair {
    u64 a, b;
};

static __attribute__((always_inline)) struct pair mk_pair(u64 a, u64 b) {
    struct pair p;
    p.a = a;
    p.b = b;
    return p;
}

struct nested {
    struct pair inner;
    u64 tail;
};

static __attribute__((always_inline)) struct nested mk_nested(u64 a, u64 b, u64 t) {
    struct nested n;
    n.inner.a = a;
    n.inner.b = b;
    n.tail = t;
    return n;
}

/* 对照 6：普通（非内联）调用，走 sret/寄存器返回路径，修复前就对。 */
static flags_t mk_flags_outline(u64 v) {
    flags_t f = (flags_t){};
    f.w[0] = v;
    return f;
}

static int checks;
static u64 g_flags;

int main(void) {
    int rc = 0;

    /* 1. 内联形参临时量的成员（8 B 结构, 数组字段首元素） */
    if (low_word(mk_flags(0x77UL)) != 0x77UL)
        rc |= 1;
    checks++;

    /* 2. 16 B 结构成员 */
    if (mk_pair(11, 22).b != 22)
        rc |= 2;
    checks++;

    /* 3. 链式成员（内联返回的结构体里再取字段） */
    if (mk_nested(3, 4, 5).inner.b != 4)
        rc |= 4;
    checks++;

    /* 4. 数组字段经成员表达式（与内核 .__vma_flags[0] 同形） */
    g_flags = mk_flags(0x118173UL).w[0];
    if (g_flags != 0x118173UL)
        rc |= 8;
    checks++;

    /* 5. 对照：复合字面量成员（修复前就正确） */
    if (mk_flags((flags_t){{0x33UL}}.w[0]).w[0] != 0x33UL)
        rc |= 16;
    checks++;

    /* 6. 对照：普通调用的成员（修复前就正确） */
    if (mk_flags_outline(0x99UL).w[0] != 0x99UL)
        rc |= 32;
    checks++;

    return checks == 6 ? rc : 64;
}
