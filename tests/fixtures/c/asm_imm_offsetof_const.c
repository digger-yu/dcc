/*
 * 内联汇编 `"i"` 约束里的整型地址常量（GCC 常见的 offsetof 写法）：
 *
 *   #define offsetof(TYPE, MEMBER) ((size_t)&((TYPE *)0)->MEMBER)
 *
 * `&((T *)0)->m` 的基址是整数常量 0，整个表达式是整型常量表达式，值 = 字段偏移。
 * 缺陷形状（-O2 走 SSA 通道）：
 *   1) 常量求值器只认「符号 + 偏移」，把空指针基址交给后端兜底成 0 ——
 *      全局初始化 `static const size_t off = offsetof(S, m);` 读回 0（gcc: 16）；
 *   2) SSA 降级把成员地址值化成 `_tsubaddr_N`，重建后操作数成了
 *      `((unsigned long)_tsubaddr_N >> 3)`，`"i"` 折不出立即数，硬报
 *      impossible constraint（-O0 走 AST 通道正常）。
 * 现场：analyzer/torture/asm-x86-linux-cpuid-paravirt-2.c 的
 * `PARAVIRT_PATCH(op)`（内核 arch/x86/include/asm/paravirt.h 同形）。
 *
 * 用返回值区分失败项，-O0..-O3 四档都要与 gcc 一致。
 */
typedef unsigned long size_t;

struct inner {
    int a;
    long b;
};

struct S {
    char pad[16];
    int m;
    struct inner in;
};

#define OFF(TYPE, MEMBER) ((size_t)&((TYPE *)0)->MEMBER)

/* 全局/静态初始化式：与 `"i"` 走同一套常量求值（缺陷 1）。 */
static const size_t off_m = OFF(struct S, m);          /* 16 */
static const size_t off_b = OFF(struct S, in.b);       /* 32 */
static const size_t off_shift = OFF(struct S, m) >> 3; /* 2 */

/* 立即数契约：值必须在 SSA 降级前折出来（缺陷 2）。 */
static int imm_m(void)
{
    unsigned long r = 0;
    __asm__("movq %1, %0" : "+r"(r) : "i"(OFF(struct S, m) / sizeof(void *)));
    return (int)r;
}

static int imm_nested(void)
{
    unsigned long r = 0;
    __asm__("movq %1, %0" : "+r"(r) : "i"(OFF(struct S, in.b)));
    return (int)r;
}

static int imm_cast_shift(void)
{
    unsigned long r = 0;
    __asm__("movq %1, %0" : "+r"(r)
            : "i"((unsigned long)&((struct S *)0)->m >> 3));
    return (int)r;
}

int main(void)
{
    if (off_m != OFF(struct S, m))
        return 1;
    if (off_b != OFF(struct S, in.b))
        return 2;
    if (off_shift != 2)
        return 3;
    if (imm_m() != 2)
        return 4;
    if (imm_nested() != 32)
        return 5;
    if (imm_cast_shift() != 2)
        return 6;
    return 0;
}
