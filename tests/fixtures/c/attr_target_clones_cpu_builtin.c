/* attr_target_clones_cpu_builtin.c —— `__builtin_cpu_supports` / `__builtin_cpu_is`
 * 是**真实现**的可证伪判据（机器无关，恒假桩必红）。
 *
 * 三条不变量，任何一条不成立就返回非 0：
 *   1) 非空锚点：x86-64 上 `sse2`（leaf1/EDX#26）恒支持、`sse`（leaf1/EDX#25）恒支持。
 *      这一条把「恒 0 桩」直接钉死 —— 它不依赖本机有什么特性。
 *   2) 架构单调性：avx2 ⇒ avx ⇒ sse4.2 ⇒ sse3 ⇒ sse2 ⇒ sse。真检测才会保持这个
 *      偏序；一位写错（例如把 leaf7 的位当成 leaf1 的位读）会以「高特性为真而低特性
 *      为假」破坏它。
 *   3) 厂商互斥：`__builtin_cpu_is("intel")` 与 `__builtin_cpu_is("amd")` 不能同时
 *      为真（leaf 0 的厂商串只能命中一种；实测本机是 GenuineIntel → intel=1、amd=0）。
 *      这一条钉的是厂商 dword 常量与拼接顺序（EBX,EDX,ECX）—— 曾经把 ECX 当 EDX 用，
 *      结果 `intel` 恒 0。
 *   4) 幂等：`__builtin_cpu_init()` 之后再问，答案不变（缓存生效且不被重算清零）。
 *
 * 打印行只用于与 gcc 逐字对照（dcc 的 builtin 返回 0/1，gcc 返回 libgcc 内部掩码，
 * 所以全部先 `!= 0` 归一 —— C 只承诺「非零 = 支持」）。
 *
 * 注意：每个调用点都必须直接写字面量。`__builtin_cpu_supports` 的参数在 gcc 与 dcc
 * 里都要求是字符串字面量（封个 `sup(const char *n)` 的辅助函数就把它变成非常量参数 → 两边都是
 * 硬错），所以下面每个调用点直接写字面量。
 */
#include <stdio.h>

int main(void) {
    int sse = __builtin_cpu_supports("sse") != 0;
    int sse2 = __builtin_cpu_supports("sse2") != 0;
    int sse3 = __builtin_cpu_supports("sse3") != 0;
    int sse42 = __builtin_cpu_supports("sse4.2") != 0;
    int avx = __builtin_cpu_supports("avx") != 0;
    int avx2 = __builtin_cpu_supports("avx2") != 0;
    int intel = __builtin_cpu_is("intel") != 0;
    int amd = __builtin_cpu_is("amd") != 0;

    printf("sse=%d sse2=%d sse3=%d sse4.2=%d avx=%d avx2=%d\n",
           sse, sse2, sse3, sse42, avx, avx2);
    printf("intel=%d amd=%d\n", intel, amd);

    __builtin_cpu_init();
    int again = __builtin_cpu_supports("sse2") != 0;
    printf("after_init sse2=%d\n", again);

    int ok = 1;
    if (sse != 1 || sse2 != 1) ok = 0;             /* 1) 非空锚点 */
    if (avx2 && !avx) ok = 0;                      /* 2) 单调性 */
    if (avx && !sse42) ok = 0;
    if (sse42 && !sse3) ok = 0;
    if (sse3 && !sse2) ok = 0;
    if (intel && amd) ok = 0;                      /* 3) 厂商互斥 */
    if (again != sse2) ok = 0;                     /* 4) 幂等 */
    return ok ? 0 : 1;
}
