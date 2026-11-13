/* __int128 元素向量与标量混合运算 / 全局 128 位向量元素读写。
 *
 * 覆盖两个历史缺陷 (对应 gcc torture pr116037 / ipa pr119318):
 *   - 逐 lane 计算时对标量操作数也加 lane 偏移, 只有第 0 个元素正确;
 *   - 全局 __int128 向量元素的 16 字节读写只搬了低 64 位;
 *   - 标量转 128 位元素时必须是符号/零扩展转换 (int -1 -> 2^128-1)。
 * -O0/-O1/-O2 都应返回 0。
 */
typedef unsigned __int128 v4u128 __attribute__((__vector_size__(64)));
typedef unsigned v16u32 __attribute__((__vector_size__(64)));

v4u128 gv = { 10, 20, 30, 40 };
v4u128 gw;
v16u32 gu = { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 };

__attribute__((noinline)) v4u128 add_scalar(unsigned long long x, v4u128 v)
{
    return x + v;
}

/* 有符号标量 -1 转 unsigned __int128 元素 -> 每个元素 2^128-1 */
__attribute__((noinline)) v4u128 add_neg(__int128 x, v4u128 v)
{
    return x + v;
}

/* 全局向量作为二元运算操作数 */
__attribute__((noinline)) v4u128 add_global(unsigned __int128 x)
{
    return x + gv;
}

int main(void)
{
    v4u128 r = add_scalar(0x1000, (v4u128){ 1, 2, 3, 4 });
    if (r[0] != 0x1001 || r[1] != 0x1002 || r[2] != 0x1003 || r[3] != 0x1004)
        return 1;

    r = add_scalar(7, gv);
    if (r[0] != 17 || r[1] != 27 || r[2] != 37 || r[3] != 47)
        return 2;

    r = add_neg(-1, (v4u128){ 0, 0, 0, 0 });
    if (r[0] != (unsigned __int128)-1 || r[3] != (unsigned __int128)-1)
        return 3;

    r = add_global(5);
    if (r[0] != 15 || r[1] != 25 || r[2] != 35 || r[3] != 45)
        return 4;

    /* 全局 128 位向量写: 每个元素的高 64 位不能丢 */
    gw = gv + 100;
    if (gw[0] != 110 || gw[3] != 140)
        return 5;

    v16u32 u = gu * 2;
    if (u[0] != 2 || u[15] != 32)
        return 6;
    return 0;
}
