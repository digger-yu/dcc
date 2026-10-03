/* target_clones：运行时**真的选中了非 default 变体**这一件事的可观察夹具。
 *
 * 为什么需要它：`attr_target_clones_select.c` 那条只能在 `.o` 侧静态钉判定链，而
 * dcc 目前没有 per-variant 的 ISA codegen（`ast.rs` 里没有 `target_attrs`），
 * 各变体的机器码**逐字节相同** —— `.o` 实测（`dcc -O0 -c attr_target_clones.c`）：
 *   `compute.sse4_2` @0x0 与 `compute.default` @0x68（st_size 都是 100，含 4B 对齐填充
 *   后各 104B）字节完全一致，
 *   `multiply.avx` @0xd0 / `.sse4_2` @0x128 / `.default` @0x180（各 82B）也两两一致。
 * 于是「选中谁」不能靠返回值差异或指令差异看出来，只能看**解析后的函数地址**。
 *
 * 这条通道在 dcc 上是真的：宿主 `ld` 为 `(void *)compute` 发一个 GOT 槽 +
 * `R_X86_64_IRELATIVE`，动态链接器调用 resolver 并把**返回值**（被选中变体的地址）
 * 写进那个槽，所以程序打印出来的就是选型结果。实测本机（有 sse4.2 与 avx，
 * `dcc -O0 attr_target_clones_selected.c -o sel.d`，经宿主 cc 以 `-no-pie` 链接）：
 *   marker=0x402310、compute.sse4_2=0x402150、compute.default=0x4021b8
 *   ⇒ 运行打 `delta=-448`，即 p=0x402150 = **compute.sse4_2**（不是 default）；
 *   dispatch3.avx=0x402220、.sse4_2=0x402270、.default=0x4022c0
 *   ⇒ 运行打 `delta3=-240`，即 q=0x402220 = **dispatch3.avx**（优先级压过 sse4_2）。
 *
 * 为什么打印**差值**而不是绝对地址：判据要能同时吃 PIE/ASLR。`marker` 是普通
 * GLOBAL FUNC，`(void *)marker` 与 `(void *)compute` 落在同一模块里，差值与加载基址
 * 无关，可以直接与 exe 符号表里 `compute.sse4_2 - marker` 比对。
 *
 * 为什么把 `__builtin_cpu_supports("sse4.2")` 的答案一起打出来：判据必须**机器无关**。
 * 在支持 sse4.2 的机器上「必须解析到 sse4_2 变体」，在不支持的机器上「必须解析到
 * default」—— 两种都只由 builtin 的读数决定。于是这三种偷工做法都会立刻红：
 *   * resolver 无条件返回 default（本机 builtin 说 1，但地址是 default）；
 *   * resolver 无条件返回第一个变体（在不支持 sse4.2 的机器上会给 sse4_2）；
 *   * `__builtin_cpu_supports` 是恒假桩（builtin 报 0，而地址仍是 sse4_2）。
 *
 * 第二个函数 `dispatch3` 是**这一条的存在理由**：任务书要求「真的选中了 avx 变体」也能
 * 被观察到。它是三变体链 `avx,sse4.2,default`，所以同时钉两件事：
 *   1. **优先级**：avx 与 sse4.2 都支持时必须选 avx（书写序也是 avx 在前，所以书写序
 *      与优先级在这一支上无法区分 —— 区分它们的是 `attr_target_clones_select.c`
 *      那条打乱书写序的 `.o` 侧判据，这里钉的是「运行时确实走到 avx 那一支」）；
 *   2. **XCR0/OSXSAVE 门控**：`avx` 在 dcc 与 gcc 里都不是「CPUID 说有就算有」，还要
 *      OS 真的开了 XMM/YMM 状态位。判据用同一个 `__builtin_cpu_supports("avx")` 的读数
 *      决定期望变体，所以「门控漏掉」与「门控写错」都会表现为地址与期望不一致。
 * 期望完全由运行时读数决定（avx → sse4.2 → default 三档），因此这条在 any 机器上都成立。
 *
 * 与 gcc 的两处有意差异（因此本夹具**不进** gcc 逐字对照；见 docs/wp_ifunc_real_fix.md）：
 *   1) `&ifunc` 的取值方式：gcc 13 把它绑到 PLT 桩（实测 `gcc -O0 -no-pie` 下
 *      `delta=-4606` ⇒ p=0x401080，落在 `.plt.sec` [0x401060,0x401090) 里），dcc 打的是
 *      解析后的实现体地址。两者调用语义相同（都跳到同一个实现），但只有 dcc 这一侧
 *      能把「选中了哪个变体」直接观察出来；
 *   2) `__builtin_cpu_supports` 的返回值：gcc 展开成 `libgcc` 内部特性字的位值
 *      （实测本机 sse4.2 打 256、avx 打 512 —— 那是 `__cpu_model` 私有枚举序的
 *      `1<<index`，随 gcc 版本变动），dcc 归一成 0/1。C/gcc 文档只承诺「非零 = 支持」，
 *      逐字对齐那个位值等于复制外部实现的私有 ABI，所以不对齐。
 * 判据本身用 `has != 0` 决定期望变体，因此对 1)/2) 都免疫。
 * 逐字对照由 `attr_target_clones_ifunc_behavior.c`（行为差异）、`attr_target_clones.c`
 * （stdout）与 `attr_target_clones_cpu_matrix.c`（60 名字 `!= 0` 归一后的全交集）承担。
 */
#include <stdio.h>

__attribute__((target_clones("sse4.2,default")))
int compute(int x) { return x * 2 + 1; }

/* 三档链：avx 优先于 sse4.2，两者都不成立才落 default。 */
__attribute__((target_clones("avx,sse4.2,default")))
int dispatch3(int x) { return x + 7; }

/* 同模块里的普通函数：给差值一个参照点。 */
int marker(void) { return 3; }

int main(void) {
    unsigned long m = (unsigned long)(void *)marker;
    int has = __builtin_cpu_supports("sse4.2");
    int has_avx = __builtin_cpu_supports("avx");
    unsigned long p = (unsigned long)(void *)compute;
    unsigned long q = (unsigned long)(void *)dispatch3;
    printf("delta=%ld has_sse42=%d call=%d\n", (long)(p - m), has, compute(10));
    printf("delta3=%ld has_avx=%d call3=%d\n", (long)(q - m), has_avx, dispatch3(10));
    return 0;
}
