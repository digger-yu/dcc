/* may_alias 类型属性综合测试
 *
 * __attribute__((may_alias)) 指示该类型的指针可以别名任何其他类型,
 * 禁用基于类型的别名分析 (TBAA)。内核中 __bitwise 类型 (如 __le32, __be32)
 * 依赖此属性进行安全的类型双关。
 */

/* 1. struct 名前的 may_alias */
struct __attribute__((__may_alias__)) aliased_struct {
    int x;
};

/* 2. struct 体后的 may_alias */
struct aliased_post {
    int y;
} __attribute__((may_alias));

/* 3. union 的 may_alias */
union __attribute__((may_alias)) aliased_union {
    int i;
    float f;
};

/* 4. enum 的 may_alias */
enum __attribute__((may_alias)) aliased_enum {
    VAL_A = 1,
    VAL_B = 2
};

/* 5. typedef 的 may_alias - 内核 __bitwise 类型风格 */
typedef unsigned int __le32 __attribute__((may_alias));
typedef unsigned int __be32 __attribute__((may_alias));
typedef unsigned short __le16 __attribute__((may_alias));
typedef unsigned short __be16 __attribute__((may_alias));
typedef unsigned long long __le64 __attribute__((may_alias));
typedef unsigned long long __be64 __attribute__((may_alias));

/* 6. 普通 typedef 带 may_alias */
typedef int my_int __attribute__((may_alias));

/* 7. 带 packed 和 may_alias 的组合 */
struct __attribute__((packed, may_alias)) packed_aliased {
    char a;
    int b;
};

/* 8. 带 aligned 和 may_alias 的组合 */
struct __attribute__((aligned(16), may_alias)) aligned_aliased {
    int data[4];
};

/* 类型双关测试: 通过 may_alias 指针安全地进行类型双关 */
int type_punning_test(void) {
    __le32 le_val = 0x12345678;
    unsigned int *ip = (unsigned int *)&le_val;
    return *ip == 0x12345678 ? 0 : 1;
}

/* struct may_alias 测试 */
int struct_alias_test(void) {
    struct aliased_struct s = { 42 };
    struct aliased_struct *p = &s;
    /* 通过 may_alias 类型指针访问 */
    return p->x == 42 ? 0 : 1;
}

/* union may_alias 测试 */
int union_alias_test(void) {
    union aliased_union u;
    u.i = 100;
    return u.i == 100 ? 0 : 1;
}

/* packed + may_alias 组合测试 */
int packed_alias_test(void) {
    struct packed_aliased ps;
    ps.a = 'A';
    ps.b = 0x12345678;
    /* packed: sizeof = 5 (1 + 4) */
    if (sizeof(struct packed_aliased) != 5) return 1;
    return ps.b == 0x12345678 ? 0 : 1;
}

/* aligned + may_alias 组合测试 */
int aligned_alias_test(void) {
    if (_Alignof(struct aligned_aliased) != 16) return 1;
    struct aligned_aliased as;
    as.data[0] = 1;
    as.data[3] = 4;
    return as.data[0] + as.data[3] == 5 ? 0 : 1;
}

int main(void) {
    int fail = 0;

    /* 基本测试 */
    fail |= type_punning_test();
    fail |= struct_alias_test();
    fail |= union_alias_test();
    fail |= packed_alias_test();
    fail |= aligned_alias_test();

    /* 枚举测试 */
    enum aliased_enum e = VAL_A;
    if (e != 1) fail |= 32;

    /* typedef 测试 */
    my_int m = 99;
    if (m != 99) fail |= 64;

    /* __le32/__be32 端序类型测试 */
    __le32 le = 0xAABBCCDD;
    __be32 be = 0x11223344;
    if (le + be != 0xAABBCCDD + 0x11223344) fail |= 128;

    return fail;
}
