/* 声明符与类型模型的矩阵：形参衰减、指针指数组、typedef、结构体成员、函数指针数组。
 *
 * 本夹具存在的原因：`int (*pa)[3]` 作为**形参**曾被解析成 Array(Ptr(Int),3)
 * （3 个 int* 的数组）而不是 Ptr(Array(Int,3))（指向 3 个 int 的数组的指针），
 * 于是 `pa[i][j]` 的步长与 `(*pa)[i]` 的衰减全错，**连 -O0 都静默错值**，
 * 而同一声明放在局部变量里是对的（真因与取证见
 * docs/ssa-pass-upgrade-plan.md §7.34）。
 *
 * 为什么每族都同时钉两件事：
 *   1) sizeof —— 直接观测类型模型本身，是最便宜的探针；
 *   2) 一次**带步长的写**落到调用方对象的指定元素 —— 因为 `Ptr(Array(T,N))` 与
 *      `Array(Ptr(T),N)` 这类反过来的类型，光看 sizeof 可能巧合相等（如 8/16），
 *      只有按正确步长写出去才算证明。
 *
 * 全部期望写在 C 里：main 打印完整掩码并只在全部成立时返回 0。注意不能用
 * `return bad` 直接交退出码——OS 只取低 8 位，位号 ≥ 256 的失败会被掩盖成 0。
 * 只比 stdout 而不要求返回 0 也不够：期望本身写错时 gcc 会配合地给出同一个值。
 */

#include <stdio.h>

/* --- 形参衰减：数组形参的类型是「指向其元素类型的指针」 --- */
static int bp1(int a[3])         { return (int)sizeof(a); }    /* -> 8  */
static int bp2(int a[3][4])      { return (int)sizeof(a); }    /* -> 8  */
static int bp3(int (*a)[4])      { return (int)sizeof(*a); }   /* -> 16 */
static int bp4(int *a[3])        { return (int)sizeof(a); }    /* -> 8  */
static int bp5(int (**a)(void))  { return (int)sizeof(*a); }   /* -> 8  */

static void st1(int a[3][4])     { a[1][0] = 111; }
static void st2(int (*a)[4])     { a[2][1] = 222; }
static void st3(int a[5])        { a[3] = 333; }
static void st4(int *a[3])       { a[1] = 0; }

/* 抽象声明符（cast / sizeof 里的 `int (*)[4]`，无变量名）：同一个结合顺序缺陷的
 * 第三条代码路径。它以前返 Array(Ptr(Int),4) —— 实测 sizeof((int(*)[4])0) 为 32
 * 而 gcc 为 8；不仅 sizeof 错，拿它当实参时连累被调函数把写落错位置
 * （`p[0][2] = 9` 写不到调用方的 m[2]）。 */
/* 故意不用 static：static + 单一调用点会被内联，而内联后走的不是「外部调用 + cast
 * 实参」那条 ABI 路径，这条回归就漏掉了（实测：改成 static 后废掉抽象声明符分支
 * 也只能报出 sizeof 那一项，`p[0][2] = 9` 写错位这一项反而抓不到）。 */
int setcell(int (*p)[4])
{
	p[0][2] = 9;
	return p[1][3];
}

/* 最初报红的真实形状 E1：实参是个**二维数组**（自己先衰减）再套一层向指针指数组的
 * 转换。与上面 `setcell((int(*)[4])m2)`（m2 是一维）不同：只有这个形状会把参数 ABI
 * 路径也带错（实测废掉抽象声明符解析时，单靠 `m2` 只报出 sizeof 那一项）。 */
int setcell3(int (*p)[4])
{
	p[1][2] = 55;
	return p[0][0];
}

/* --- 指针指数组的 typedef（曾解析不了，报 E0004/E0001/E0003） --- */
typedef int  (*arrp)[3];
typedef char (*rowp)[16];

/* --- 函数指针数组 / 指向函数指针数组的指针 --- */
static int  f0(void) { return 7; }

int main(void)
{
    int m[3][4];
    int v[5];
    int *q[3];
    int buf[32];
    char cbuf[40];
    int (*fp[2])(void);
    int bad = 0;
    int i;
    int j;

    for (i = 0; i < 3; i++) {
        for (j = 0; j < 4; j++) {
            m[i][j] = i * 10 + j;
        }
    }
    for (i = 0; i < 5; i++) {
        v[i] = i;
    }
    for (i = 0; i < 3; i++) {
        q[i] = v + i;
    }
    for (i = 0; i < 32; i++) {
        buf[i] = 0;
    }

    /* sizeof：类型模型本身 */
    if (bp1(m[0]) != 8)          bad |= 1;
    if (bp2(m) != 8)             bad |= 2;
    if (bp3(m) != 16)            bad |= 4;
    if (bp4(q) != 8)             bad |= 8;
    if (bp5(0) != 8)             bad |= 16;
    if ((int)sizeof(m) != 48)    bad |= 32;
    if ((int)sizeof(m[0]) != 16) bad |= 64;
    if ((int)sizeof(arrp) != 8)      bad |= 128;
    if ((int)sizeof(fp) != 16)       bad |= 256;
    /* 抽象声明符（cast 类型）的尺寸：只能是指针 8。位号不与上面 512（st1）重用，
     * 否则一次失败会被误读成另一处失败。 */
    if ((int)sizeof((int (*)[4]) 0) != 8) bad |= 262144;

    /* 带步长的写：必须落在调用方对象的正确元素上 */
    st1(m);  if (m[1][0] != 111) bad |= 512;
    st2(m);  if (m[2][1] != 222) bad |= 1024;
    st3(v);  if (v[3] != 333)    bad |= 2048;
    st4(q);  if (q[1] != 0)      bad |= 4096;
    /* 指针指数组的 typedef：行步长 3 与 16，写入位置是唯一能分开正反结合的读数 */
    {
        arrp t = (arrp) buf;
        rowp c = (rowp) cbuf;
        if ((int)sizeof(*t) != 12)   bad |= 8192;
        t[0][0] = 42;
        if (buf[0] != 42)            bad |= 16384;
        t[1][2] = 43;
        if (buf[5] != 43)            bad |= 32768;
        if ((int)sizeof(*c) != 16)   bad |= 65536;
        c[1][3] = 7;
        if (cbuf[19] != 7)           bad |= 131072;
    }

    /* 函数指针数组的元素必须是可调用指针；调不对就返回错值 */
    fp[0] = f0;
    fp[1] = f0;
    if (fp[0]() + fp[1]() != 14)    bad |= 2097152;

    /* 抽象声明符不仅影响 sizeof：拿它当实参时，被调方按指针指数组算出的
     * 地址必须落在调用方的对象上（实测修前 `p[0][2] = 9` 写不到 m2[2]）。 */
    {
        int m2[8] = {0, 1, 2, 3, 4, 5, 6, 7};
        if (setcell((int (*)[4]) m2) != 7) bad |= 4194304;
        if (m2[2] != 9)                    bad |= 8388608;
    }

    /* E1 形状：二维数组实参 + 向指针指数组的转换 */
    {
        int big[3][4];
        for (i = 0; i < 3; i++) {
            for (j = 0; j < 4; j++) {
                big[i][j] = i * 10 + j;
            }
        }
        if (setcell3((int (*)[4]) big) != 0) bad |= 16777216;
        if (big[1][2] != 55)                 bad |= 33554432;
    }

    /* 退出码会被 OS 只取低 8 位（`return 262144` 在 shell 里读成 0），所以不能
     * 直接 `return bad`：那样位号 ≥ 256 的失败全部看不见。这里把完整掩码打印
     * 出来供诊断，并只返回 0/1 作为自检结果。 */
    printf("bad=%d\n", bad);
    return bad == 0 ? 0 : 1;
}
