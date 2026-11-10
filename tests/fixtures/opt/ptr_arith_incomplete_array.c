/* tests/fixtures/opt/ptr_arith_incomplete_array.c
 *
 * 指针算术的运算宽度只能由「操作数是不是地址」决定，不能由 `Type::size()` 推：
 *
 *   * 不完整数组 `T a[]` 在 AST 里是 `Array(_, Const(0))`，`size()` 是 **0**；
 *   * 下标是无符号（或枚举——`Type::is_int()` 不含枚举，`is_signed_int()` 也说
 *     它不有符号）时，`op_size <= 4 && is_unsigned` 同时成立，
 *     `binop()` 就把 `arr + i` 发成 `add %r8d,%eax` —— 32 位加法把 64 位基址的
 *     高 32 位清零。
 *
 * 现场（报告 §5.20）：内核 `kernel/cpu.c` 的
 *   static struct cpuhp_step cpuhp_hp_states[];           // 前置不完整声明
 *   static inline struct cpuhp_step *cpuhp_get_step(enum cpuhp_state state)
 *   { return cpuhp_hp_states + state; }
 * 在 `__cpuhp_setup_state_cpuslocked` 里被编成
 *   lea cpuhp_hp_states,%rax ; movslq %edi,%r8 ; imul $0x28,%r8,%r8 ; add %r8d,%eax
 * `add %r8d,%eax` 把 0xffffffff87c5db00 截成 0x87c5db00，加 0x4b0 得 0x87c5dfb0；
 * 紧接着 `mov (%rax),%rax` 读线性地址 0x87c5dfb0（用户空间、未映射）触发 #PF，
 * 早期异常栈上再叠 #DF，QEMU 记 `Triple fault`，serial 停在
 * `Policy zone: DMA32`（kbuild9，0.51s）。
 *
 * 形状与内核同源：`struct step` 与 `struct cpuhp_step` 同尺寸（40 字节：name +
 * 三个指针槽 + hlist_head + 两个 bool 补齐），数组前置声明为不完整类型，
 * 定义在函数之后。判据分两半：
 *   1. 地址值：`(ulong)(arr + i)` 必须等于 `(ulong)arr + i * sizeof(*arr)` ——
 *      不解引用，缺修复时也不会崩，退出码可读；
 *   2. 真读一次 `arr + i` 的成员（内核就是读 `sp->name`）—— 缺修复时是野指针，
 *      这条会直接崩，与内核现场同形。
 *
 * 反方向也钉住：完整数组（`size()` 非 0）与真指针两种形态本来就是对的，
 * 不许为了修这条把它们改成别的宽度；`i + arr`（交换律那一臂）与
 * `p - arr`（指针差）一并覆盖。
 */

typedef unsigned long ulong;

/* 40 字节，与内核 struct cpuhp_step 同尺寸。 */
struct step {
    const char *name;
    void *a;
    void *b;
    void *c;
    unsigned char f1;
    unsigned char f2;
};

/* 前置不完整声明：Array(_, Const(0))。 */
static struct step hp_states[];

/* 完整数组：Array(_, Const(8))，size() = 320，走的是另一条判据。 */
static struct step full_states[8];

enum st { ST0, ST1, ST2, ST3, ST4, ST5, ST6, ST7 };

/* 内核形状：`return cpuhp_hp_states + state;`（下标是枚举）。 */
static struct step *hp_step(enum st s) {
    return hp_states + s;
}

/* 同一条算式，下标是无符号整数。 */
static struct step *hp_step_u(unsigned s) {
    return hp_states + s;
}

/* 交换律那一臂：指针在右。 */
static struct step *hp_step_rev(unsigned s) {
    return s + hp_states;
}

/* 结果先落到指针变量再读 —— SSA 重建后的**结果类型**也要是 64 位地址。 */
static const char *name_via_ptr(enum st s) {
    struct step *p = hp_states + s;
    return p->name;
}

static int failures;

static void check(int ok, int bit) {
    if (!ok) {
        failures |= bit;
    }
}

static struct step hp_states[] = {
    {"s0", 0, 0, 0, 0, 0},
    {"s1", 0, 0, 0, 0, 0},
    {"s2", 0, 0, 0, 0, 0},
    {"s3", 0, 0, 0, 0, 0},
    {"s4", 0, 0, 0, 0, 0},
    {"s5", 0, 0, 0, 0, 0},
    {"s6", 0, 0, 0, 0, 0},
    {"s7", 0, 0, 0, 0, 0},
};

static struct step full_states[8] = {
    {"f0", 0, 0, 0, 0, 0},
    {"f1", 0, 0, 0, 0, 0},
    {"f2", 0, 0, 0, 0, 0},
    {"f3", 0, 0, 0, 0, 0},
    {"f4", 0, 0, 0, 0, 0},
    {"f5", 0, 0, 0, 0, 0},
    {"f6", 0, 0, 0, 0, 0},
    {"f7", 0, 0, 0, 0, 0},
};

int main(void) {
    /* 1: 不完整数组 + 无符号下标 —— 地址值（§5.20 现场） */
    check((ulong)hp_step_u(3) == (ulong)hp_states + 3 * sizeof(struct step), 1);
    /* 2: 不完整数组 + 枚举下标 —— 内核 cpuhp_get_step 的原始形状 */
    check((ulong)hp_step(ST5) == (ulong)hp_states + 5 * sizeof(struct step), 2);
    /* 4: 真读一次（内核读 sp->name），无符号下标 */
    check(hp_step_u(5)->name[0] == 's' && hp_step_u(5)->name[1] == '5', 4);
    /* 8: 结果先落到指针变量（SSA 重建的结果类型也要是地址） */
    check(name_via_ptr(ST6)[0] == 's' && name_via_ptr(ST6)[1] == '6', 8);
    /* 16: 交换律那一臂 `i + arr` */
    check((ulong)hp_step_rev(2) == (ulong)hp_states + 2 * sizeof(struct step), 16);
    /* 32: 完整数组 + 无符号下标（本来就是对的，反向钉住） */
    check((ulong)(full_states + 4u) == (ulong)full_states + 4 * sizeof(struct step), 32);
    /* 64: 真指针 + 无符号下标（反向钉住） */
    check((ulong)(hp_states + 7u) == (ulong)hp_states + 7 * sizeof(struct step), 64);
    /* 128: 指针差 —— 截断过的基址会让 (arr + 7) - arr 变成天文数字 */
    check((hp_states + 7) - hp_states == 7, 128);

    return failures;
}
