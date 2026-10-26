/* 回归：AST GVN 的别名链必须一次压平（`dt -> vt -> target`）。
 *
 * `p2` 里 `aux(list, target, 255, target, out)` 让 `vt` 与 `dt` 同时别到
 * `target`。修复前 `coalesce_block` 只替换一层，随后删掉声明，`dtarget`
 * 形参拿到未初始化的栈槽 —— `-O2` 打印 `p2 4 0 -9 18` 而不是 `p2 4 18 -9 18`。
 */
#include <stdio.h>

static void aux(int list, int vt, int reg, int dt, int *out) {
    if (reg != 255) out[2] = reg;
    out[0] = list;
    out[1] = dt;
    out[3] = vt;
}

/* 三个调用点分别覆盖「不同名」「同名」「换位」三种别名形态。 */
void p1(int list, int target, int other, int *out) { aux(list, target, 255, other, out); }
void p2(int list, int target, int other, int *out) { aux(list, target, 255, target, out); }
void p3(int list, int target, int other, int *out) { aux(list, other, 255, target, out); }

int main(void) {
    int o[4] = {-9, -9, -9, -9};
    p1(4, 18, 77, o);
    printf("p1 %d %d %d %d\n", o[0], o[1], o[2], o[3]);
    int q[4] = {-9, -9, -9, -9};
    p2(4, 18, 77, q);
    printf("p2 %d %d %d %d\n", q[0], q[1], q[2], q[3]);
    int r[4] = {-9, -9, -9, -9};
    p3(4, 18, 77, r);
    printf("p3 %d %d %d %d\n", r[0], r[1], r[2], r[3]);
    return (o[1] == 77 && q[1] == 18 && r[1] == 18) ? 0 : 1;
}
