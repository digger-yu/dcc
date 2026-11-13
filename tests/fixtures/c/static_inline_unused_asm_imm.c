/*
 * `static inline` 的独立副本在没人引用时不该发射（GCC 语义，-O0 实测同样如此：
 * 未使用的普通 `static` 会发射，`static inline` 一个符号都不留）。
 *
 * 缺陷形状：
 *   * dcc 的跨函数 DCE 只在 -O2 起跑，-O0/-O1 把 `static inline` 副本照样发射；
 *   * `__always_inline` 助手的调用点全被展开后，副本体里的 `"i"(形参)` 离开
 *     调用点的常量实参根本折不出立即数 ⇒ 整个 TU 硬错误
 *     （`impossible constraint in 'asm' ... (expression: k)`）。
 * 现场：analyzer/torture/asm-x86-linux-cpuid-paravirt-2.c 与
 * asm-x86-linux-wfx_get_ps_timeout-full.c 的 `arch_static_branch`（内核
 * include/linux/jump_label.h 同形；这两个用例本身 dg-skip-if -O0，因为 GCC
 * 在 -O0 也折不了 `"i"(形参)` —— 本夹具只要求「无人引用的副本不发射」，
 * 与 GCC 一样在 -O0 可编）。
 *
 * 反向对照：
 *   * 地址被取的 `static inline` 必须留下副本且可调用；
 *   * 仍有 out-of-line 调用点的 `static inline`（-O0 不会被内联）必须留副本。
 * 用返回值区分失败项，-O0..-O3 四档都要与 gcc 一致。
 */
typedef unsigned long size_t;

/* 无人调用：体里的 "i"(k) 只有独立副本才会被求值 —— 副本必须被丢弃。 */
static inline __attribute__((__always_inline__)) int imm_unused(int k)
{
    unsigned long r = 0;
    __asm__("movq %1, %0" : "+r"(r) : "i"(k));
    return (int)r;
}

/* 调用点全部内联（立即数有独立真源）：副本同样没人引用，应当一并丢弃。 */
static inline __attribute__((__always_inline__)) int imm_lit(void)
{
    unsigned long r = 0;
    __asm__("movq %1, %0" : "+r"(r) : "i"(6));
    return (int)r;
}

/* 地址被取：必须有 out-of-line 副本，否则链接期 undefined reference。 */
static inline int add_one(int x)
{
    return x + 1;
}
static int (*pick_add)(int) = add_one;

/* 普通 static inline：-O0 的调用点不会被内联，副本必须保留。 */
static inline int mul_two(int x)
{
    return x * 2;
}

int use_lit(void)
{
    return imm_lit();
}

int use_mul(int x)
{
    return mul_two(x);
}

int main(void)
{
    if (use_lit() != 6)
        return 1;
    if (pick_add(41) != 42)
        return 2;
    if (use_mul(21) != 42)
        return 3;
    return 0;
}
