/* tests/fixtures/asm/cmpxchg16b_double.c
 *
 * 内核 arch/x86/include/asm/cmpxchg_64.h::__arch_cmpxchg128 同形：
 *
 *   asm_inline volatile("lock cmpxchg16b %[ptr]"
 *                : [ptr] "+m" (*(_ptr)),
 *                  "+a" (o.low), "+d" (o.high)
 *                : "b" (n.low), "c" (n.high)
 *                : "memory");
 *
 * 这条指令有两个容易一起犯的错（报告 §5.20）：
 *
 *   1. `0F C7 /1` 不带 REX.W 就是 **cmpxchg8b**（比较 EDX:EAX、交换 8 字节），
 *      带着 REX.W 才是 cmpxchg16b（比较 RDX:RAX、交换 16 字节）。旧实现的
 *      两项都是 `rex(false, ..)`，objdump 反汇编成
 *      `f0 40 0f c7 0b  lock rex cmpxchg8b (%rbx)`。
 *   2. `"m"(*ptr)` 的地址暂存（scratch）在固定约束预扫描**之前**分配，于是
 *      挑中 `"b"` 约束要钉死的 `%rbx`：`mov ptr,%rbx` 之后紧接
 *      `mov n_lo,%rbx`，指令在 n_lo 的**值**上寻址。现场里 n_lo 是 0，
 *      于是读线性地址 0，QEMU 在 `PageBuddy` 附近 triple fault。
 *
 * 判据分两半：
 *
 *   1. 机器码/反汇编（`cmpxchg16b_double_is_rexw_and_avoids_fixed_regs`）：
 *      两条发射路径（`-c` 直发对象、`-S`→GAS）都必须恰好一条 `cmpxchg16b`，
 *      不得出现 `cmpxchg8b`，且内存基址不得是 a/b/c/d 四个固定约束钉住的
 *      号（rax/rbx/rcx/rdx）。
 *   2. 运行语义（`cmpxchg16b_double_runtime_matches_gcc`，CPU 有 cx16）：
 *      成功路径返回比较值且低/高 64 位**都**换成新值；失败路径返回内存
 *      当前值且内存原封不动。高 64 位这一条只有真 16 字节指令才成立。
 *
 * 对齐：cmpxchg16b 要求 16 字节对齐，违例是 #GP，所以 cell 显式对齐。
 */

typedef unsigned long ulong;

typedef struct {
    ulong lo;
    ulong hi;
} halves;

/* 返回比较前的 RDX:RAX：成功时等于 old，失败时等于内存当前值。 */
static __attribute__((noinline)) void dcc_cmpxchg128(volatile halves *ptr, halves old, halves neu, halves *seen) {
    halves o = old;
    halves n = neu;
    __asm__ volatile("lock cmpxchg16b %[ptr]"
                     : [ptr] "+m" (*ptr), "+a"(o.lo), "+d"(o.hi)
                     : "b"(n.lo), "c"(n.hi)
                     : "memory");
    *seen = o;
}

static halves cell __attribute__((aligned(16)));

static int failures;

static void check(int ok, int bit) {
    if (!ok) {
        failures |= bit;
    }
}

int main(void) {
    halves got;
    halves old;
    halves neu;

    /* ---- 成功路径：内存 == old，必须换成 new ---- */
    cell.lo = 0x1111111122222222UL;
    cell.hi = 0x3333333344444444UL;
    old.lo = 0x1111111122222222UL;
    old.hi = 0x3333333344444444UL;
    neu.lo = 0x5555555566666666UL;
    neu.hi = 0x7777777788888888UL;

    dcc_cmpxchg128(&cell, old, neu, &got);
    /* 1: 成功时 RDX:RAX 保持比较值 */
    check(got.lo == old.lo && got.hi == old.hi, 1);
    /* 2: 低 64 位已换 */
    check(cell.lo == neu.lo, 2);
    /* 4: 高 64 位也换了 —— cmpxchg8b 只动低 8 字节，这一位会红 */
    check(cell.hi == neu.hi, 4);

    /* ---- 失败路径：内存 != old，必须原封不动 ---- */
    cell.lo = 0xaaaaaaaaaaaaaaaaUL;
    cell.hi = 0xbbbbbbbbbbbbbbbbUL;

    dcc_cmpxchg128(&cell, old, neu, &got);
    /* 8: 失败时返回内存当前值 */
    check(got.lo == 0xaaaaaaaaaaaaaaaaUL && got.hi == 0xbbbbbbbbbbbbbbbbUL, 8);
    /* 16: 失败时内存完全不动（高 64 位没被半写） */
    check(cell.lo == 0xaaaaaaaaaaaaaaaaUL && cell.hi == 0xbbbbbbbbbbbbbbbbUL, 16);

    return failures;
}
