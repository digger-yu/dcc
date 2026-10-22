/* 条件直生分支（P13 阶段3）的**行为**夹具：比较结果只用于决定分支时，发射层只发
 * `cmp`/`test` + 一条 `jcc`，不再走 `setcc + movzbq + test` 的 0/1 材料化。
 *
 * 判据的来源是产物级快照 `tests/disasm_snapshots_x86_64/direct_cond_branch`（形状锁），
 * 本夹具锁的是**语义**：条件码与极性若在任何一档漂了（有符号/无符号搞反、逆条件忘取反、
 * 令牌泄到嵌套比较导致读垃圾 rax），这里是第一个抓住它的产物级检查。
 *
 * 覆盖的形态：
 *   直生：有符号 `<`/`>=`、无符号 `>`/`<=`、`unsigned char` 提升后的比较、指针与 0、
 *         三元、`do-while`、`while`、`for`；
 *   必须仍材料化（放行判据白名单之外）：浮点比较、位域读入的比较、把比较结果当值用
 *         （`int r = (a==b)`、`(a<b)+1`）；
 *   短路副作用：`f() && g()` 里 `g` 在 `f` 为假时不得被调用（直生不许把两条分支合并掉）。
 *
 * 通过: exit 0，且末行输出 `direct_cond_branch: OK`（四档 -O0/-O1/-O2/-O3 与 gcc 一致）。
 */

#include <stdio.h>

struct bf {
    unsigned int a : 3;
};

static int signed_j(int a, int b) {
    if (a < b) return 1;
    if (a >= b) return 2;
    return 3;
}

static unsigned unsigned_j(unsigned a, unsigned b) {
    if (a > b) return 1;
    if (a <= b) return 2;
    return 3;
}

static int narrow_j(unsigned char c) {
    if (c > 200) return 1;
    return 0;
}

static int ptr_j(const int *p) {
    if (p != 0) return 1;
    if (p == 0) return 2;
    return 3;
}

static int ternary_j(int a, int b) { return a < b ? 11 : 22; }

static int dowhile_j(int n) {
    int s = 0;
    do {
        s += n;
        n--;
    } while (n > 0);
    return s;
}

static int while_j(int n) {
    int s = 0;
    while (n > 0) {
        s += n;
        n--;
    }
    return s;
}

static int for_j(int n) {
    int s = 0;
    for (int i = 0; i < n; i++) s += i;
    return s;
}

static int float_j(double d, double e) {
    if (d < e) return 1;
    return 0;
}

static int bitfield_j(struct bf *b) {
    if (b->a == 3) return 1;
    return 0;
}

static int value_j(int a, int b) {
    int r = (a == b);
    int t = (a < b) + 1;
    return r * 10 + t;
}

static int g_calls;

static int g_yes(void) {
    g_calls++;
    return 1;
}

static int short_circuit_j(int n) {
    g_calls = 0;
    int r = 0;
    if (n > 0 && g_yes()) r = 1; /* n<=0 时 g_yes 不得被调用 */
    r += (n > 100) ? 2 : 3;
    return r * 10 + g_calls;
}

#define EXPECT(name, got, want)                                                                  \
    do {                                                                                         \
        if ((got) != (want)) {                                                                   \
            fails++;                                                                             \
            printf("FAIL %s: got %d want %d\n", (name), (int)(got), (int)(want));               \
        }                                                                                        \
    } while (0)

int main(void) {
    int fails = 0;
    struct bf b;
    b.a = 3;
    const int xs[] = { -2147483647 - 1, -5, -1, 0, 1, 5, 2147483647 };

    /* 有符号 / 无符号 / 三元：极值与相等边界 */
    EXPECT("signed(min,min)", signed_j(xs[0], xs[0]), 2);
    EXPECT("signed(min,max)", signed_j(xs[0], xs[6]), 1);
    EXPECT("signed(max,min)", signed_j(xs[6], xs[0]), 2);
    EXPECT("signed(-5,-1)", signed_j(-5, -1), 1);
    EXPECT("signed(-1,-5)", signed_j(-1, -5), 2);
    EXPECT("unsigned(min,min)", (int)unsigned_j((unsigned)xs[0], (unsigned)xs[0]), 2);
    EXPECT("unsigned(min,max)", (int)unsigned_j((unsigned)xs[0], (unsigned)xs[6]), 1);
    EXPECT("unsigned(max,min)", (int)unsigned_j((unsigned)xs[6], (unsigned)xs[0]), 2);
    EXPECT("unsigned(-1u,0u)", (int)unsigned_j(4294967295u, 0u), 1);
    EXPECT("ternary(eq)", ternary_j(7, 7), 22);
    EXPECT("ternary(lt)", ternary_j(-1, 0), 11);

    /* 窄类型提升后是 int 比较（不是无符号 char 比较） */
    EXPECT("narrow(200)", narrow_j(200), 0);
    EXPECT("narrow(201)", narrow_j(201), 1);
    EXPECT("narrow(255)", narrow_j(255), 1);

    /* 指针与 0 */
    EXPECT("ptr(null)", ptr_j(0), 2);
    EXPECT("ptr(valid)", ptr_j(xs), 1);

    /* 循环三种形态的回程条件（do-while 是「真则回头」，极性反了会死循环或少跑） */
    EXPECT("dowhile(0)", dowhile_j(0), 0);
    EXPECT("dowhile(1)", dowhile_j(1), 1);
    EXPECT("dowhile(4)", dowhile_j(4), 10);
    EXPECT("dowhile(-3)", dowhile_j(-3), -3);
    EXPECT("while(0)", while_j(0), 0);
    EXPECT("while(4)", while_j(4), 10);
    EXPECT("while(-3)", while_j(-3), 0);
    EXPECT("for(0)", for_j(0), 0);
    EXPECT("for(5)", for_j(5), 10);
    EXPECT("for(-1)", for_j(-1), 0);

    /* 白名单外：仍走材料化，值必须对 */
    EXPECT("float(lt)", float_j(1.5, 2.5), 1);
    EXPECT("float(eq)", float_j(3.0, 3.0), 0);
    EXPECT("bitfield(3)", bitfield_j(&b), 1);
    b.a = 2;
    EXPECT("bitfield(2)", bitfield_j(&b), 0);
    EXPECT("value(eq)", value_j(4, 4), 11);
    EXPECT("value(lt)", value_j(1, 2), 2); /* r=0, t=2 → 2 */
    EXPECT("value(gt)", value_j(5, 1), 1);  /* r=0, t=1 → 1 */

    /* 短路：条件直生不得让被短路的一侧真的被执行 */
    EXPECT("shortcircuit(-1)", short_circuit_j(-1), 30);
    EXPECT("shortcircuit(5)", short_circuit_j(5), 41);
    EXPECT("shortcircuit(200)", short_circuit_j(200), 31);

    if (fails == 0) printf("direct_cond_branch: OK\n");
    return fails;
}
