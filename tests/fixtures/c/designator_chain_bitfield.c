/* 链式设计符指向位域：`.bits.p = 1` 必须按 bit_offset 打包进存储单元。
 *
 * 旧实现两处（全局静态初始化 elf/init_list.rs、运行时 gen_init_list）都只对
 * 单层 `.field` 取位域信息，链式设计符落进普通标量写：值放在字段起始字节、
 * 丢掉单元内移位。内核 arch/x86/kernel/idt.c 的 def_idts[] 正是这个形状，
 * IDT 门的 p 位为 0，第一个 #UD 即 #GP(0x0032) → #DF → triple fault。
 *
 * 失败码：
 *   1 = 全局 packed 位域链 .p（应为 bit15）
 *   2 = 全局 packed 位域链 .type（应为 bit8）
 *   3 = 全局 packed 位域链 .ist（bit0..2）
 *   4 = 全局 ISTG 形态（ist+1，应 0x8E02 语义）
 *   5 = 运行时局部变量同一形状
 *   6 = 静态局部变量同一形状
 *   7 = 非 packed 位域容器的链式设计符（.s.c bit8 / .s.e bit15）
 *   8 = 链式设计符指向普通嵌套字段（回归护栏，不能被打包逻辑影响）
 *   9 = 经 union 成员的链式设计符
 *  10 = 数组元素后再链式取位域（a[2].bits.p）
 */
struct idt_bits {
    unsigned short ist : 3, zero : 5, type : 5, dpl : 2, p : 1;
} __attribute__((packed));

struct idt_data {
    unsigned int vector;
    unsigned int segment;
    struct idt_bits bits;
    const void *addr;
};

#define G(_v, _a, _ist, _type, _dpl, _seg)                                     \
    {                                                                          \
        .vector = _v, .bits.ist = _ist, .bits.type = _type, .bits.dpl = _dpl,  \
        .bits.p = 1, .addr = _a, .segment = _seg                               \
    }
#define INTG(_v, _a) G(_v, _a, 0, 0xE, 0, 0x10)
#define ISTG(_v, _a, _ist) G(_v, _a, _ist + 1, 0xE, 0, 0x10)

static const struct idt_data table[] = {
    INTG(0, (void *)0x1000),
    ISTG(2, (void *)0x2000, 1),
    ISTG(8, (void *)0x3000, 0),
};

/* 非 packed 容器：位域跨字节共用存储单元（bit_offset 可 > 7） */
struct wide_bits {
    unsigned short a : 3, b : 5, c : 5, d : 2, e : 1;
};
struct wide_holder {
    unsigned x;
    struct wide_bits s;
};

static struct wide_holder wide = {
    .x = 0xAA,
    .s.c = 0x0A, /* bit8..12 */
};
static struct wide_holder wide_e = {
    .x = 0xAA,
    .s.e = 1, /* bit15 */
};

/* 普通嵌套字段（不是位域）：链式设计符的回归护栏 */
struct inner {
    unsigned short a;
    unsigned short b;
};
struct outer {
    unsigned x;
    struct inner s;
    unsigned z;
};
static struct outer plain = {.x = 1, .s.a = 0x2122, .s.b = 0x3132, .z = 2};

/* 经 union 成员 */
struct uni_holder {
    union {
        struct idt_bits bits;
        unsigned short raw;
    } u;
};
static struct uni_holder uni = {.u.bits.ist = 1, .u.bits.type = 0xE, .u.bits.p = 1};

/* 数组元素再链式位域 */
static struct idt_data arr[3] = {
    [0] = INTG(0, (void *)0x1000),
    [2] = {.vector = 3, .bits.p = 1},
};

int printf(const char *, ...);

static int check_global(void)
{
    if (table[0].bits.p != 1 || table[0].bits.type != 0xE || table[0].bits.ist != 0)
        return 1;
    if (table[0].bits.dpl != 0)
        return 2;
    if (table[1].bits.ist != 2)
        return 3;
    if (table[2].bits.ist != 1)
        return 4;
    return 0;
}

static int check_runtime(void)
{
    struct idt_data l1 = {.vector = 6,
                          .segment = 0x10,
                          .bits.ist = 1,
                          .bits.type = 0xE,
                          .bits.dpl = 0,
                          .bits.p = 1,
                          .addr = (void *)0x2000};
    static struct idt_data l2 = {.vector = 7,
                                 .segment = 0x10,
                                 .bits.ist = 0,
                                 .bits.type = 0xE,
                                 .bits.dpl = 1,
                                 .bits.p = 1,
                                 .addr = (void *)0x2000};
    if (l1.bits.p != 1 || l1.bits.ist != 1 || l1.bits.type != 0xE)
        return 5;
    if (l2.bits.p != 1 || l2.bits.dpl != 1 || l2.bits.ist != 0)
        return 6;
    return 0;
}

static int check_wide(void)
{
    if (wide.s.c != 0x0A)
        return 7;
    if (wide.s.a != 0 || wide.s.b != 0)
        return 7;
    if (wide_e.s.e != 1)
        return 7;
    return 0;
}

static int check_plain(void)
{
    if (plain.s.a != 0x2122 || plain.s.b != 0x3132 || plain.x != 1 || plain.z != 2)
        return 8;
    return 0;
}

static int check_union(void)
{
    if (uni.u.bits.p != 1 || uni.u.bits.ist != 1 || uni.u.bits.type != 0xE)
        return 9;
    return 0;
}

static int check_array(void)
{
    if (arr[0].bits.p != 1 || arr[0].bits.type != 0xE)
        return 10;
    if (arr[2].bits.p != 1 || arr[2].vector != 3)
        return 10;
    return 0;
}

int main(void)
{
    int rc;
    if ((rc = check_global()))
        return rc;
    if ((rc = check_runtime()))
        return rc;
    if ((rc = check_wide()))
        return rc;
    if ((rc = check_plain()))
        return rc;
    if ((rc = check_union()))
        return rc;
    if ((rc = check_array()))
        return rc;
    printf("designator-chain-bitfield ok\n");
    return 0;
}
