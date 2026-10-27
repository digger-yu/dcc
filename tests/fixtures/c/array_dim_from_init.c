/* 不完整数组的维度只由初始化器决定（C11 6.7.9p22），而这条推导必须对**所有声明符
 * 形状**一致：`int t[]`、`char s[]` 这类平面数组声明符历史上会推导，而经括号声明符
 * 构造出来的 `static int (*ext[])(void) = {…}`（元素类型是指向函数的指针）走的是另一
 * 条分支，维度停在 `Const(0)` —— 对象尺寸算成 0，`sizeof(ext)/sizeof(ext[0])` 也得 0。
 * SQLite 的 `static int (*const sqlite3BuiltinExtensions[])(sqlite3*) = {…}` 正是这一形，
 * 长度为 0 意味着内置扩展一个都不注册，产物里 `sqlite_dbpage` 一类虚表不存在。
 *
 * 期望（与 gcc 逐字节对照）：
 *   ext_n=3 acc=7 scalars_n=4 sum=17 msg_n=7 local_n=2 local_acc=3
 */
#include <stdio.h>

static int fa(void) { return 1; }
static int fb(void) { return 2; }
static int fc(void) { return 4; }

static int (*const ext[])(void) = { fa, fb, fc };
static int scalars[] = { 7, 8, 9, 10 };
static char msg[] = "sqlite";

int main(void) {
    unsigned n = (unsigned)(sizeof(ext) / sizeof(ext[0]));
    unsigned m = (unsigned)(sizeof(scalars) / sizeof(scalars[0]));
    unsigned k = (unsigned)sizeof(msg);
    int acc = 0;
    for (unsigned i = 0; i < n; i++) {
        acc += ext[i]();
    }
    /* 同一形状出现在块作用域里也必须被推导 */
    int (*local[])(void) = { fa, fb };
    unsigned ln = (unsigned)(sizeof(local) / sizeof(local[0]));
    int lacc = 0;
    for (unsigned i = 0; i < ln; i++) {
        lacc += local[i]();
    }
    printf("ext_n=%u acc=%d scalars_n=%u sum=%d msg_n=%u local_n=%u local_acc=%d\n",
           n, acc, m, scalars[0] + scalars[m - 1], k, ln, lacc);
    return 0;
}
