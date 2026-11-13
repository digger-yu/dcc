/* __builtin_{add,sub}_overflow_p 语义: 操作数提升为无限精度有符号,
 * 结果 cast 到第三个参数的类型; cast 截断则返回 1。
 *
 * 覆盖 pr117476-2: ~g 是 128 位操作数而目标只有 64 位, 只按目标
 * 宽度计算会把高 64 位整体忽略, 把 2^128-1 判成"不溢出"。
 */
unsigned __int128 g;

static int add128_to_ul(unsigned __int128 a, unsigned __int128 b)
{
    return __builtin_add_overflow_p(a, b, 0ul);
}

static int sub128_to_ul(unsigned __int128 a, unsigned __int128 b)
{
    return __builtin_sub_overflow_p(a, b, 0ul);
}

static int add128_to_ll(__int128 a, __int128 b)
{
    return __builtin_add_overflow_p(a, b, 0ll);
}

static int add128_to_uc(unsigned __int128 a)
{
    return __builtin_add_overflow_p(a, 0, (unsigned char)0);
}

int main(void)
{
    unsigned __int128 one = 1;
    unsigned __int128 big = one << 64;

    /* pr117476-2 原样: ~g 全 1, unsigned long 放不下 */
    if (__builtin_add_overflow_p(~g, 0, 0ul) != 1)
        return 1;
    if (add128_to_ul(0, 0) != 0)
        return 2;
    if (add128_to_ul(0xffffffffffffffffULL, 0) != 0)
        return 3;
    if (add128_to_ul(big, 0) != 1)
        return 4;
    if (add128_to_ul(0xffffffffffffffffULL, 1) != 1)
        return 5;
    if (add128_to_ul(5, 7) != 0)
        return 6;
    if (sub128_to_ul(100, 200) != 1)
        return 7;
    if (sub128_to_ul(200, 100) != 0)
        return 8;
    if (add128_to_ll(-5, 3) != 0)
        return 9;
    if (add128_to_ll(0x7fffffffffffffffLL, 1) != 1)
        return 10;
    if (add128_to_uc(200) != 0)
        return 11;
    if (add128_to_uc(300) != 1)
        return 12;
    return 0;
}
