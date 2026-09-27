/* 缺陷 6 夹具: 全局二维数组的行下标被塌缩.
 *
 * 现象 (修复前): `static long long e[10][2];` 逐行写再逐行读, 多行读出同一个
 * 元素的内容 (行 stride 塌缩成 0 的等价失效形态); 指针写法 `(*(e + 2))[1]`
 * 恒读到 `(*(e + 2))[0]`。
 * 根因 (两级, 均已收口):
 *  1) 地址描述符 `ByteOffset` 只有**一个**运行时下标槽 (`Runtime { stride,
 *     addend }`) —— `add_byte_offsets` 把 `Constant + Runtime` 合并时不分层。
 *     当链上有一层下标是编译期常量、另一层是运行时表达式 (`e[i][1]` /
 *     `e[3][j]`), 物化阶段直接把「本层 idx」当那个运行时下标喂进去, 常量 1 乘上
 *     行 stride 后把真正的 `i` 整丢, 内层下标被抹掉。改为 `Runtime { terms,
 *     addend }` 逐项累加描述符, 不再需要任何调用点猜「哪个下标是运行时的」。
 *  2) `mem/access.rs::emit_address_descriptor` 对 `AddressBase::ExprAddr` (即
 *     `*(...)` 形态) 直接 `return` 递归结果, 跳过了本层的常量偏移加法, 于是
 *     内层常量下标 (`[1]` 的 +8) 被静默丢弃。改为与 `PointerValue` 臂同构的
 *     fall-through, 偏移统一由 Step 2 单点发射。
 * 两条判据都落在**物化端**, 不在任何调用点按夹具/表达式形态特判。
 *
 * 覆盖轴: 2D/3D/4D / 元素宽度 1,2,4,8 字节 / 非 2 次幂内层维度 (7x3, 5x9) /
 * 下标常量+变量混合 (e[i][1], e[3][j], e[i][j]) / 行指针退化
 * (long long (*p)[2]) / 指针算术 ((*(e+2))[1] 与 *(e[3]+1) 一致性) /
 * 负下标 / 静态初始化器读回 / auto 存储期对照 / sizeof 维度账 / 校验和。
 */
#include <stdio.h>

static long long e10x2[10][2];
static int small7x3[7][3];
static double dbl5x9[5][9];
static char ch3x4x5[3][4][5];
static short sh2x2x2x2[2][2][2][2];
static const int init2d[4][3] = { { 1, 2, 3 }, { 4, 5, 6 }, { 7, 8, 9 },
    { 10, 11, 12 } };
static long long gp10x2[10][2]; /* 外部链接 (global) 对照 static */

static int bad;

#define CHK(label, got, want)                                                   \
    do {                                                                        \
        long long g_ = (long long)(got);                                        \
        long long w_ = (long long)(want);                                       \
        if (g_ != w_) {                                                         \
            printf("FAIL %-16s got=%lld want=%lld\n", (label), g_, w_);         \
            bad++;                                                              \
        } else {                                                                \
            printf("ok   %-16s = %lld\n", (label), g_);                         \
        }                                                                       \
    } while (0)

int main(void)
{
    /* 1. 缺陷现场: static long long e[10][2] 写满再读回 */
    for (int i = 0; i < 10; i++) {
        e10x2[i][0] = i * 100L;
        e10x2[i][1] = i * 100L + 1L;
    }
    long long sum0 = 0, sum1 = 0;
    for (int i = 0; i < 10; i++) {
        sum0 += e10x2[i][0];
        sum1 += e10x2[i][1];
    }
    CHK("e10x2.sum0", sum0, 4500);
    CHK("e10x2.sum1", sum1, 4510);
    /* 行内容必须互不相同 (塌缩成同一行时这里会连着错) */
    for (int i = 0; i < 10; i++) {
        CHK("e10x2.row.i", e10x2[i][0] / 100, i);
    }
    printf("rows distinct=%d\n", e10x2[0][0] != e10x2[9][0]);
    if (e10x2[0][0] == e10x2[9][0]) {
        bad++;
    }

    /* 2. 非 2 次幂行宽: int[7][3] (行 stride 12 字节) */
    for (int i = 0; i < 7; i++) {
        for (int j = 0; j < 3; j++) {
            small7x3[i][j] = i * 10 + j;
        }
    }
    CHK("small7x3[6][2]", small7x3[6][2], 62);
    CHK("small7x3[0][2]", small7x3[0][2], 2);
    CHK("small7x3[3][0]", small7x3[3][0], 30);
    CHK("small7x3[3][2]", small7x3[3][2], 32);
    CHK("small7x3[6][0]", small7x3[6][0], 60);
    int cs = 0;
    for (int i = 0; i < 7; i++) {
        for (int j = 0; j < 3; j++) {
            cs += small7x3[i][j];
        }
    }
    CHK("small7x3.csum", cs, 651);

    /* 3. double[5][9]: 变眼下标 (下标来自变量, 不能被常量折叠掩盖) */
    for (int i = 0; i < 5; i++) {
        for (int j = 0; j < 9; j++) {
            dbl5x9[i][j] = (double)(i * 100 + j);
        }
    }
    int vi = 3, vj = 7;
    CHK("dbl5x9[v][v]", (long long)dbl5x9[vi][vj], 307);
    CHK("dbl5x9[0][8]", (long long)dbl5x9[0][8], 8);
    CHK("dbl5x9[4][0]", (long long)dbl5x9[4][0], 400);
    /* 下标常量+变量混合的三种形 (旧实现只错其一并静默丢下标) */
    CHK("dbl5x9[v][c]", (long long)dbl5x9[vj % 5][2], 202);
    CHK("dbl5x9[c][v]", (long long)dbl5x9[2][vj], 207);
    CHK("dbl5x9[c][c]", (long long)dbl5x9[4][8], 408);
    double ds = 0.0;
    for (int i = 0; i < 5; i++) {
        ds += dbl5x9[i][4];
    }
    CHK("dbl5x9.col4", (long long)ds, 4 + 104 + 204 + 304 + 404);

    /* 4. 3D char[3][4][5]: 每层 stride 都要算对 */
    for (int i = 0; i < 3; i++) {
        for (int j = 0; j < 4; j++) {
            for (int k = 0; k < 5; k++) {
                ch3x4x5[i][j][k] = (char)(i * 20 + j * 5 + k + 1);
            }
        }
    }
    CHK("ch3x4x5[2][3][4]", ch3x4x5[2][3][4], 60);
    CHK("ch3x4x5[0][0][0]", ch3x4x5[0][0][0], 1);
    CHK("ch3x4x5[1][2][3]", ch3x4x5[1][2][3], 34);
    /* 3D: 中间层常量 / 中间层变量 各一组 */
    CHK("ch3x4x5[1][v][3]", ch3x4x5[1][vj % 4][3], 1 * 20 + 3 * 5 + 3 + 1);
    CHK("ch3x4x5[v][2][v]", ch3x4x5[vi % 3][2][vi % 5],
        (vi % 3) * 20 + 2 * 5 + (vi % 5) + 1);
    long csum = 0;
    for (int i = 0; i < 3; i++) {
        for (int j = 0; j < 4; j++) {
            for (int k = 0; k < 5; k++) {
                csum += ch3x4x5[i][j][k];
            }
        }
    }
    CHK("ch3x4x5.csum", csum, 1830);

    /* 5. 4D short[2][2][2][2] */
    for (int i = 0; i < 2; i++) {
        for (int j = 0; j < 2; j++) {
            for (int k = 0; k < 2; k++) {
                for (int l = 0; l < 2; l++) {
                    sh2x2x2x2[i][j][k][l] = (short)(i * 8 + j * 4 + k * 2 + l);
                }
            }
        }
    }
    CHK("sh4d[1][1][1][1]", sh2x2x2x2[1][1][1][1], 15);
    CHK("sh4d[0][1][0][1]", sh2x2x2x2[0][1][0][1], 5);
    CHK("sh4d[1][0][1][0]", sh2x2x2x2[1][0][1][0], 10);

    /* 6. 行指针退化与指针算术必须与下标写法一致 */
    long long (*p)[2] = e10x2;
    CHK("p[i][1]", p[5][1], 501);
    CHK("p[0][0]", p[0][0], 0);
    CHK("(e+2)[1]", (*(e10x2 + 2))[1], 201);
    CHK("(e+3)[1]", (*(e10x2 + 3))[1], 301);
    CHK("(e+0)[1]", (*(e10x2 + 0))[1], 1);
    CHK("e[3][1]eq", e10x2[3][1] == *(e10x2[3] + 1), 1);
    CHK("rowptr.deref", *(&e10x2[7][0] + 1), 701);
    /* 行的列指针 (一级退化, 元素是 int) */
    int *col = small7x3[4];
    CHK("colptr[2]", col[2], 42);

    /* 7. 负下标 / 表达式下标 */
    long long *mid = &e10x2[6][1];
    CHK("neg-index", mid[-1], 600);
    CHK("expr-index", e10x2[2 + 3][0], 500);
    CHK("mul-index", e10x2[1 * 4][1], 401);

    /* 8. 静态初始化器读回 (const 二维) */
    CHK("init2d[0][0]", init2d[0][0], 1);
    CHK("init2d[1][2]", init2d[1][2], 6);
    CHK("init2d[3][0]", init2d[3][0], 10);
    CHK("init2d[3][2]", init2d[3][2], 12);
    CHK("init2d[2][1]", init2d[2][1], 8);

    /* 9. auto 存储期对照 (缺陷只在 static/global 侧发作) */
    long long a2[4][2];
    for (int i = 0; i < 4; i++) {
        a2[i][0] = i * 7;
        a2[i][1] = i * 7 + 1;
    }
    CHK("auto2d[3][1]", a2[3][1], 22);
    CHK("auto2d[0][1]", a2[0][1], 1);

    /* 10. global (外部链接) 同形状数组 */
    for (int i = 0; i < 10; i++) {
        gp10x2[i][0] = 10 - i;
        gp10x2[i][1] = 100 - i;
    }
    CHK("gp10x2[0][0]", gp10x2[0][0], 10);
    CHK("gp10x2[9][0]", gp10x2[9][0], 1);
    CHK("gp10x2[9][1]", gp10x2[9][1], 91);

    /* 11. sizeof 维度账 (stride 推导的另一半真源) */
    CHK("sizeof.e10x2", (long long)sizeof(e10x2), 160);
    CHK("sizeof.row", (long long)sizeof(e10x2[0]), 16);
    CHK("rows.by.sizeof", (long long)(sizeof(e10x2) / sizeof(e10x2[0])), 10);
    CHK("sizeof.3d", (long long)sizeof(ch3x4x5), 60);

    printf("bad=%d\n", bad);
    return bad;
}
