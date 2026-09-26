/* ld80_vararg_callee.c 的调用方。
 *
 * 由 **gcc** 编译（`--mix` 协议：dcc callee + gcc caller），因此任何偏离都只能
 * 来自 dcc 的读侧 —— 这正是本用例存在的意义：dcc 自己调自己是自洽的错，
 * 单 TU 差分查不出来。
 */
extern long long AI, BI;
extern int AD;
extern int f(int, ...);

int main(void) {
    f(1, 3.25L, 2.5L);
    /* 判据走退出码，逐项独立，不用 sum-N（mod-256 会别名成 0） */
    if (AI != 3)
        return 1;
    if (BI != 2)
        return 2;
    if (AD != 2)
        return 3;
    return 0;
}
