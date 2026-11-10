/* tests/fixtures/opt/clz_width_by_builtin_name.c
 *
 * 前导零计数的**宽度**只能从内建名来：`__builtin_clz` 是 32 位，`clzl`/`clzll`
 * 在 x86_64 上都是 64 位（`long` 与 `long long` 同为 8 字节，GCC 的这两个原型
 * 就是 `unsigned long` / `unsigned long long`）。实参是裸字面量时 `Operand::Int`
 * 不带类型，SSA 重建只能按量级阶梯猜（`3ULL` 被猜成 `int`），于是
 * `64 - __builtin_clzll(3ULL)` 在 -O2 得 34（应 2）—— 比真值大 32，恰好就是
 * 32 位口径的差。
 *
 * 现场是内核 `mm/sparse.c` 的 `mem_section_usage_size()`：
 *   roundup_pow_of_two(4) → 1UL << fls64(3) → 1UL << (64 - __builtin_clzll(3))
 * 少了 32 位 ⇒ `1UL << 34`，usemap 向 memblock 申请 1 TB 失败（`memblock=debug`
 * 记的是 `1099511627968 bytes`），随后 `sparse_init_subsection_map` 在
 * `ms->section_mem_map` 的 usage 为 NULL 时取 `->map`（+0x10），PVH 启动在
 * `__bitmap_set+0x183` 以 #GP 收场（报告 §5.18）。
 *
 * 形状与内核同源：`fls64()` 抄 `arch/x86/include/asm/bitops.h`（CONFIG_X86_64 的
 * 常量那支就是 `64 - __builtin_clzll(x)`），`roundup_pow_of_two()` 抄
 * `include/linux/log2.h`。用宏而不是函数，保证内建拿到的确实是字面量。
 *
 * 反方向也钉住：`__builtin_clz` 必须仍是 32 位口径（不能为了修这条把三兄弟一起
 * 改成 64 位），实参为**变量**时没有阶梯可猜，也不许倒退。
 */

typedef unsigned long ulong;
typedef unsigned long long ullong;

static int failures;

/* include/linux/log2.h 的 roundup_pow_of_two，内建的那一步原样保留。 */
#define fls64_of(x) ((x) ? 64 - __builtin_clzll(x) : 0)
#define roundup_pow_of_two(n) (1UL << fls64_of((n) - 1UL))

/* arch/x86/include/asm/bitops.h: CONFIG_X86_64 的 fls64（同一条内建，走变量路径）。 */
static int fls64(ullong x) {
    return x ? 64 - __builtin_clzll(x) : 0;
}

/* 静态初始化走 const_eval 那条路，口径必须与发射端一致。 */
static const int clzll_const_init = 64 - __builtin_clzll(3ULL);

static void check(int ok, int bit) {
    if (!ok) {
        failures |= bit;
    }
}

int main(void) {
    /* 1: 字面量实参下的 64 位宽度（§5.18 现场） */
    check(64 - __builtin_clzll(3ULL) == 2, 1);
    check((int)(63 - __builtin_clzl(3UL)) == 1, 2);
    check(__builtin_clzll(1ULL) == 63, 4);
    check(__builtin_clzl(0x100000000UL) == 31, 8);

    /* 2: 无后缀仍是 32 位口径（这条不能被顺手改坏） */
    check(__builtin_clz(3u) == 30, 16);
    check(__builtin_clz(1u) == 31, 32);

    /* 3: 内核形状 —— roundup_pow_of_two(4) 必须是 4，不是 1<<34 */
    check(roundup_pow_of_two(4) == 4, 64);
    check(roundup_pow_of_two(5) == 8, 128);
    check(fls64_of(3) == 2, 256);
    check(fls64_of(0x100000000ULL) == 33, 512);

    /* 4: 实参是变量时不得倒退 */
    ullong v = 3;
    ulong w = 0x100000000UL;
    check(64 - __builtin_clzll(v) == 2, 1024);
    check((int)(63 - __builtin_clzl(w)) == 32, 2048);
    check(fls64(3) == 2, 4096);

    /* 5: 静态初始化的那一眼 */
    check(clzll_const_init == 2, 8192);

    return failures;
}
