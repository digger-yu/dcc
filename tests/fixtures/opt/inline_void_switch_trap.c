/* 强制内联拼接 + 常量 switch 折叠的语义夹具。
 *
 * 背景（`report/linux71-dcc-fulltree-build-2026-10-28.md` §3.13–§3.16）：内核那一族
 * 「编译期尺寸检查」宏长这样
 *     #define __raw_cmpxchg(ptr, old, new, size, lock) ({ … switch (size) {
 *         case 1: …; break; … default: __cmpxchg_wrong_size(); } … })
 * 调用点传的是常量 `sizeof(*(ptr))`。这些陷阱函数**故意不定义**，完全指望编译器把
 * 不可达的 `default:` 臂消除掉；gcc 连 -O0 都能折掉，dcc 此前不能，于是
 * `ld vmlinux` 报上千行 `undefined reference to '__cmpxchg_wrong_size'`。
 *
 * 本夹具覆盖四类必须同时成立的形状：
 *   A 拼接一个体内无 `return` 的 `__always_inline` void 助手，其 `switch(形参)` 的实参是
 *     常量 —— 这条正是「死出口标签把 caller 踢出 SCCP 通道」的现场（旧行为：default 臂
 *     的 `trap_void()` 被真的发进产物）。
 *   B 真 fall-through 链（常量选择子命中后**没有** break 的臂必须继续跑进下一臂）。
 *   C 花括号臂 `case 8: { …; break; }`（`break` 不在臂的顶层）。
 *   D 带标签的臂 + `goto`（折叠的保守面：不许把有人跳的标签删掉）。
 *
 * 判据：四档 (-O0/-O1/-O2/-O3) 的 stdout 与退出码都必须与 gcc 一致（A 的 trap 分支一旦被
 * **错折成可达**，`trap_ran` 就不再是 0，运行期立刻红）。
 * 「不可达的 `default:` 是否连引用都不发」不在本夹具里判 —— 那要靠重定位条目，
 * 见集成用例 `forced_inline_void_switch_leaves_no_trap_relocation`。
 *
 * 注意：`trap_void` 在这里**是有定义的**。因为 `run_gcc_cross_check_opt` 的参照系是
 * `gcc -O0`，而 gcc -O0 会把 `default:` 臂连调用一起发出去；拿一个故意不定义的符号做
 * 夹具会让参照侧先链接失败，测不到想测的东西。
 */
#include <stdio.h>

int trap_ran = 0;
void trap_void(void) { trap_ran = 1; }

/* A: void 助手 + 常量实参 + 无 return */
static inline __attribute__((always_inline)) void sw_void(int size, int *out)
{
    switch (size) {
    case 1: *out = 1; break;
    case 4: *out = 42; break;
    case 8: *out = 8; break;
    default: trap_void(); *out = -1; break;
    }
}

/* C: 花括号臂，break 在块里 */
static long sw_braced(const volatile long *p, long o)
{
    long ret = 0;
    switch (sizeof(*p)) {
    case 4: { ret = o + 1; break; }
    case 8: { ret = o + 2; break; }
    default: ret = o + 100; break;
    }
    return ret;
}

/* B: 真 fall-through 链（常量选择子） */
static int chain_const(void)
{
    int r = 0;
    switch (1) {
    case 1: r += 1; /* fallthrough */
    case 2: r += 10; break;
    default: r += 100;
    }
    return r;
}

/* B': 真 fall-through 链（变量选择子，不许被折坏） */
static int chain_var(int k)
{
    int r = 0;
    switch (k) {
    case 1: r += 10; /* fallthrough */
    case 2: r += 20; break;
    case 3: r += 30;
    }
    return r;
}

/* D: 带标签的臂 + goto（保守面：标签有人跳就不能动） */
static int labeled(int k)
{
    switch (k) {
    case 1: goto L;
    case 2: return 7;
    L: return 3;
    default: return 4;
    }
}

int main(void)
{
    int v = 0;
    long x = 0;
    /* sizeof(long) 在 LP64 上是 8；这里故意用 4 字段的助手形状（int 尺寸）验证选择子折叠 */
    sw_void((int)sizeof(int), &v);
    printf("A %d\n", v);
    printf("C %ld\n", sw_braced(&x, 5));
    printf("B %d\n", chain_const());
    printf("B' %d %d %d\n", chain_var(1), chain_var(2), chain_var(3));
    printf("D %d %d %d\n", labeled(1), labeled(2), labeled(3));
    /* A 的陷阱分支必须不可达：选到了 case 4，`trap_ran` 永远不该变 1 */
    printf("T %d\n", trap_ran);
    return 0;
}
