/* tests/fixtures/c/int128_member_init.c
 *
 * `__int128` 作为**聚合成员/元素**时，初始化式必须走唯一一条 store 口径
 * （`Codegen::store_i128_from_rax_rdx`）：`emit_store` 对 size 不在 {1,2,4,8}
 * 的值**不发任何指令**（整块静默丢失，而不是截断），struct/union 字段分支
 * 曾漏掉这一臂 —— 成员只剩前面的零初始化。
 *
 * 现场（报告 §5.22）：内核 `arch/x86/include/asm/cmpxchg_64.h` 的
 *   union __u128_halves o = { .full = *(_oldp) }, n = { .full = (_new) };
 * 在 mm/slub.c 的 `__slab_update_freelist` 里，`o`/`n` 的 16 字节 store 被丢掉，
 * `lock cmpxchg16b` 的比较值/新值恒为 0:0，永远失败；boot 卡在失败返回路径的
 * `mfence` 上原地重试（gdb 三次采样 RIP 同一个地址），串口停在
 * `mem auto-init` 之后。
 *
 * 失败码（成功返回 0；`assert_all_levels_match_gcc` 要求两侧退出码相等且为 0）：
 *   1  指定初始化器 + union 成员（内核形状，源是 128 位指针解引用）
 *   2  指定初始化器 + struct 成员
 *   3  位置初始化器 `{ *p }`
 *   4  常量初始化器
 *   5  窄无符号源：高半零扩展
 *   6  窄有符号源：高半符号扩展
 *   7  数组同口径回归（`__int128 a[2] = {3, 5}`）
 *   8  内核调用形状：每圈重读内存的 try_cmpxchg 循环必须首圈成功
 *      （老代码里这个循环永远不收敛 —— 夹具用 8 圈上限把它变成可观测的失败；
 *      cmpxchg16b 的 16 字节对齐要求用全局 aligned(16) 对象满足，
 *      与内核 `struct slab` 里 `offsetof(freelist_counters) % 16 == 0` 的
 *      static_assert 同形）
 */

typedef unsigned long ulong;
typedef unsigned __int128 u128;

union halves {
    u128 full;
    struct {
        ulong lo, hi;
    };
};
struct box {
    u128 full;
};

#define HI 0x1111111111111111UL
#define LO 0x2222222222222222UL

static union halves cell = { .full = ((u128)HI << 64) | LO };
static union halves slot __attribute__((aligned(16)));

__attribute__((noinline)) static int t_union(u128 *p) {
    union halves o = { .full = *p };
    return o.lo == LO && o.hi == HI;
}

__attribute__((noinline)) static int t_struct(u128 *p) {
    struct box b = { .full = *p };
    return (ulong)b.full == LO && (ulong)(b.full >> 64) == HI;
}

__attribute__((noinline)) static int t_positional(u128 *p) {
    union halves o = { *p };
    return o.lo == LO && o.hi == HI;
}

__attribute__((noinline)) static int t_const(void) {
    union halves o = { .full = (u128)0x123456789abcdef0UL };
    return o.lo == 0x123456789abcdef0UL && o.hi == 0;
}

__attribute__((noinline)) static int t_zext(unsigned x) {
    struct box b = { .full = x };
    return (ulong)b.full == 0x12345678UL && (ulong)(b.full >> 64) == 0;
}

__attribute__((noinline)) static int t_sext(int x) {
    struct box b = { .full = x };
    return (ulong)b.full == (ulong)(long)x && (ulong)(b.full >> 64) == ~0UL;
}

__attribute__((noinline)) static int t_array(void) {
    u128 a[2] = { 3, 5 };
    return (ulong)a[0] == 3 && (ulong)(a[0] >> 64) == 0 && (ulong)a[1] == 5;
}

/* 与内核 `__arch_try_cmpxchg128` 同形：`=@ccz` + `"+m"` + `"+a"`/`"+d"` + `"b"`/`"c"`。 */
__attribute__((noinline)) static int try_swap(u128 *mem, u128 *expected, u128 desired) {
    union halves o = { .full = *expected }, n = { .full = desired };
    _Bool ok;
    __asm__ __volatile__("lock cmpxchg16b %[ptr]"
                         : "=@ccz"(ok), [ptr] "+m"(*mem), "+a"(o.lo), "+d"(o.hi)
                         : "b"(n.lo), "c"(n.hi)
                         : "memory");
    if (!ok)
        *expected = o.full;
    return ok;
}

/* 调用方每圈重读内存（get_from_partial_node 的 do-while 形状）。 */
__attribute__((noinline)) static int t_kernel_shape(void) {
    int i;
    slot.full = cell.full;
    for (i = 0; i < 8; i++) {
        u128 expected = slot.full;
        if (try_swap(&slot.full, &expected, (u128)0x123456789UL))
            return slot.full == (u128)0x123456789UL;
    }
    return 0;
}

int main(void) {
    if (!t_union(&cell.full))
        return 1;
    if (!t_struct(&cell.full))
        return 2;
    if (!t_positional(&cell.full))
        return 3;
    if (!t_const())
        return 4;
    if (!t_zext(0x12345678u))
        return 5;
    if (!t_sext(-12345))
        return 6;
    if (!t_array())
        return 7;
    if (!t_kernel_shape())
        return 8;
    return 0;
}
