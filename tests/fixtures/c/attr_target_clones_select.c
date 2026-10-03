/* attr_target_clones_select.c —— target_clones「选型优先级」的可证伪夹具。
 *
 * 为什么需要它（`attr_target_clones.c` 不够）：那份夹具只有 sse4.2/avx 两档，
 * 既不能区分「按 ISA 优先级排链」与「按属性书写顺序排链」，也没有 `static` 基名，
 * 于是 `IFUNC LOCAL` + `FUNC LOCAL <base>.resolver` 那一支形状没人钉。
 *
 * 这里的两个基名故意把书写顺序打乱（`sse4.2,avx512f,avx,default` /
 * `default,avx,sse4.2`）：dcc 与 gcc 都必须按**特性优先级表**（真源
 * `semantics::cpu_isa`，实测 gcc 13 对 `("sse4.2","avx")` 与 `("avx","sse4.2")`
 * 生成同一条 avx→sse4.2 判定链）排 resolver 的分支，所以判据是「resolver 体内
 * PC32 重定位命中的变体符号序列」，写死在测试里：
 *     pick        : avx512f → avx → sse4.2 → default
 *     pick_static : avx → sse4.2 → default
 * 任何一处回到「按声明顺序」或「只认 default」都会立刻红。
 */
#include <stdio.h>

__attribute__((target_clones("sse4.2,avx512f,avx,default")))
int pick(int x) { return x * 3 + 1; }

/* `static` 基名：resolver 也必须是 LOCAL（与 gcc 13 实测同形）。 */
__attribute__((target_clones("default,avx,sse4.2")))
static int pick_static(int x) { return x * 3 + 2; }

int main(void) {
    printf("pick=%d\n", pick(1));
    printf("pick_static=%d\n", pick_static(1));
    /* builtin 与 resolver 共用同一条 `emit_feature_check`：把本机答案打出来，
     * 与上面 resolver 判定链的首项对照（判据在测试里做）。gcc 的 builtin 返回
     * libgcc 内部掩码（实测 sse4.2=256 / avx=512 / avx2=1024 / avx512f=16777216），
     * C 只承诺「非零 = 支持」，所以两边都先 `!= 0` 归一。 */
    printf("supports=%d%d%d%d\n",
           __builtin_cpu_supports("sse4.2") != 0,
           __builtin_cpu_supports("avx") != 0,
           __builtin_cpu_supports("avx2") != 0,
           __builtin_cpu_supports("avx512f") != 0);
    return 0;
}
