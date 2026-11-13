/* __builtin_bswap128 (GCC 14): 16 字节字节序反转。
 * 运行期路径与 parser 常量折叠路径必须给出同一结果。
 */
typedef union {
    struct { unsigned long long lo, hi; } s;
    unsigned __int128 n;
} u128;

const u128 U1 = { .s = { 0x1122334455667788ULL, 0xffffffffffffffffULL } };
const u128 U2 = { .s = { 0xffffffffffffffffULL, 0x8877665544332211ULL } };

int main(void)
{
    /* 运行期: 64 位构造函数 + 常量初始化的 union 成员 */
    if (__builtin_bswap128(U1.n) != U2.n)
        return 1;

    /* 常量折叠: 小值只占最低 2 字节, 反转后落到最高 2 字节 */
    unsigned __int128 x = __builtin_bswap128(0x1122);
    if (x != ((unsigned __int128)0x2211000000000000ULL << 64))
        return 2;

    /* 常量折叠: 低 64 位非零, 反转后进高 64 位 */
    if (__builtin_bswap128((unsigned __int128)0x8877665544332211ULL) !=
        ((unsigned __int128)0x1122334455667788ULL << 64))
        return 3;
    return 0;
}
