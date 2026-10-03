/* attr_target_clones_ifunc_behavior.c —— 「CPUID 真被跑过、真选中的不是 default」
 * 的**行为级**可证伪夹具。
 *
 * 为什么不能拿 `target_clones` 做行为判据：dcc 目前没有 per-variant 的 ISA codegen
 * （`ast.rs` 里没有 `target_attrs`），所以 `foo.avx2` 与 `foo.default` 的机器码完全
 * 相同，选中谁都无法在 stdout 里看出来。`(void *)foo` 在 dcc 上确实能拿到**解析后的
 * 实现体地址**（经 GOT + `R_X86_64_IRELATIVE`，判据见
 * `target_clones_runtime_resolves_to_non_default_variant` 与夹具
 * `attr_target_clones_selected.c`），但那只是「地址相等」，拿不到**行为差异**；本
 * 夹具用 `__attribute__((ifunc))` + **两个返回值不同的实现体**把「选型真发生」钉成
 * 可观察的返回值差别（与 gcc 逐字对照）。「判定链顺序」则在 `.o` 侧静态钉
 * （见 `attr_target_clones_select.c` 那条测试）。
 *
 * 两者共用同一套机器：`__builtin_cpu_supports` 与合成 resolver 都走
 * `codegen::x86_64::ifunc::emit_feature_check` 与同一个 `__dcc_cpu_indicator_init`，
 * 所以「ifunc 的选型真发生」等价地证明了那套检测不是恒假桩。
 *
 * 判据为什么用 `sse2`：x86-64 上 leaf1/EDX#26 恒真，于是「非 default 实现必被选中」
 * 是机器无关的断言 —— 恒假 resolver / 无条件返回 default 的实现只能得到 1。
 * 第二个 ifunc 用 `avx2`（本机真、老机器假），只与 gcc 逐字比输出，不写死结果。
 */
#include <stdio.h>

static int slow_impl(void) { return 1; }
static int fast_impl(void) { return 2; }
static int avx2_on_impl(void) { return 11; }
static int avx2_off_impl(void) { return 12; }

/* resolver 由动态链接器在 `.init` 之前调用，所以必须自己保证检测已跑过。
 * gcc 的做法是 `.init_array` 里的 `__cpu_indicator_init` + 显式
 * `__builtin_cpu_init()`；dcc 的合成体是幂等的（`INIT_DONE` 槽守卫，见
 * `codegen::x86_64::ifunc::emit_cpu_indicator_init`），这里仍按 gcc 文档要求显式
 * 调一次 —— 两边同形，也避免依赖「谁先跑」。 */
static void *resolve_sse2(void) {
    __builtin_cpu_init();
    return __builtin_cpu_supports("sse2") ? (void *)fast_impl : (void *)slow_impl;
}

static void *resolve_avx2(void) {
    __builtin_cpu_init();
    return __builtin_cpu_supports("avx2") ? (void *)avx2_on_impl : (void *)avx2_off_impl;
}

int by_sse2(void) __attribute__((ifunc("resolve_sse2")));
int by_avx2(void) __attribute__((ifunc("resolve_avx2")));

int main(void) {
    int a = by_sse2();
    int b = by_avx2();
    /* 运行时再问一次：resolver 的选型与 builtin 的答案必须同源同真值。 */
    int want_a = __builtin_cpu_supports("sse2") != 0 ? 2 : 1;
    int want_b = __builtin_cpu_supports("avx2") != 0 ? 11 : 12;
    printf("by_sse2=%d want_a=%d\n", a, want_a);
    printf("by_avx2=%d want_b=%d\n", b, want_b);
    /* x86-64 上 sse2 恒支持：`a` 必须是 2（选中非 default 实现）。 */
    return (a == 2 && a == want_a && b == want_b) ? 0 : 1;
}
