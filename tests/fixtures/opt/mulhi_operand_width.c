/* tests/fixtures/opt/mulhi_operand_width.c
 *
 * `(unsigned) * (u64)字面量` 再 `>> 32` —— 内核 `lib/vsprintf.c` 的 `put_dec_full8`
 * 就用这一形做十进制拆分（`q = (r * (u64)0x28f5c29) >> 32;` 即 `r / 100` 的免除法写法）。
 * SSA 往返把 `(u64)` 折叠成裸 `Operand::Int` 后，重建只能按量级阶梯猜类型，乘法被压回
 * 32 位口径，发射序成 `imul; mov %eax,%eax; shr $32`：先截低 32 位再右移 32 恒得 0。
 * 现场是 PVH 启动在 `put_dec_full8+0x8a` 以 #DF 收场（报告 §5.17）。
 *
 * 同时钉住两条不能被这张表误伤的形状：有符号一侧的宽度、以及指针算术里的裸常量
 * `1` 不得被刷成指针型。
 */
typedef unsigned long long u64;

static unsigned pairs_checked;

/* 与内核同源的那一步：r < 10^8，q = r / 100（免除法）。 */
static unsigned div100_by_magic(unsigned r) {
    return (unsigned)((r * (u64)0x28f5c29ULL) >> 32);
}

static void put_dec_pair(char *buf, unsigned r) {
    unsigned q = div100_by_magic(r);
    unsigned rem = r - 100 * q;
    buf[0] = (char)('0' + rem / 10);
    buf[1] = (char)('0' + rem % 10);
    buf[2] = 0;
}

int main(void) {
    static const unsigned vals[] = {
        0u, 1u, 9u, 10u, 11u, 99u, 100u, 101u, 4294u, 9999u, 10000u,
        1234567u, 12345678u, 40394747u, 99999999u,
    };
    char b[4];

    for (unsigned i = 0; i < sizeof vals / sizeof vals[0]; ++i) {
        unsigned r = vals[i];
        /* 免除法写法必须与真除法一致（r < 10^8 时恒成立）。 */
        if (div100_by_magic(r) != r / 100u) {
            return 1;
        }
        put_dec_pair(b, r);
        if (b[0] != (char)('0' + (r % 100u) / 10u) || b[1] != (char)('0' + (r % 100u) % 10u)) {
            return 2;
        }
        pairs_checked++;
    }

    /* 有符号一侧：`long long` 乘积的 `>> 32` 不能被压成 32 位。 */
    long long s = 0x12345678LL * 0x100000000LL;
    if ((long long)(s >> 32) != 0x12345678LL) {
        return 3;
    }
    unsigned long long u = 0xdeadbeefcafeULL;
    if ((unsigned)(u >> 32) != 0xdeadu) {
        return 4;
    }

    /* 指针算术：`arr + 1 + 1` 里的裸常量 `1` 不得被按指令槽位的类型表刷成指针型。 */
    int arr[4] = {1, 2, 3, 4};
    int *p = arr + 1 + 1;
    if (*p != 3) {
        return 5;
    }
    /* 指针差仍要是 ptrdiff_t（`long`），不能退回 32 位。 */
    long d = (long)(p + 1) - (long)arr;   /* p 是 &arr[2]，p+1 是 &arr[3] ⇒ 3 个元素 */
    if (d != 3 * (long)sizeof(int)) {
        return 6;
    }

    return pairs_checked == 15 ? 0 : 7;
}
