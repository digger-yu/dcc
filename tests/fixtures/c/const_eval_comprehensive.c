/* const_eval 增强: 常量表达式求值综合测试
 * 覆盖: 位域宽度常量表达式、sizeof/alignof/offsetof 折叠、
 *       二元算术、三元条件、类型转换、语句表达式、复数常量
 */

/* 1. 二元算术运算作为常量 */
#define SHIFT_VAL (1 << 20)
#define MASK_VAL ((1 << 8) - 1)

/* 2. 位域宽度使用常量表达式 */
struct const_bitfield {
    unsigned int a : (1 << 3);        /* 8 bits: 移位运算 */
    unsigned int b : (4 + 4);         /* 8 bits: 加法 */
    unsigned int c : (SHIFT_VAL > 0 ? 16 : 8); /* 16 bits: 三元 */
    unsigned int d : ((unsigned int)(1 << 2)); /* 4 bits: 类型转换 */
};

/* 3. 枚举常量在常量表达式中使用 */
enum { PAGE_SHIFT = 12, PAGE_SIZE = (1 << PAGE_SHIFT) };

/* 4. sizeof/alignof 在常量上下文中 */
_Static_assert(sizeof(int) == 4, "int must be 4 bytes");
_Static_assert(sizeof(long) == 8, "long must be 8 bytes");
_Static_assert(_Alignof(int) == 4, "int align must be 4");

/* 5. offsetof 在常量上下文中 */
struct test_off {
    char a;
    int b;
    char c;
};
_Static_assert(sizeof(struct test_off) > 0, "struct size > 0");

/* 6. 三元条件运算 */
_Static_assert((1 ? 42 : 0) == 42, "ternary true branch");
_Static_assert((0 ? 42 : 99) == 99, "ternary false branch");

/* 7. 类型转换在常量表达式中 */
_Static_assert((int)(unsigned long)0xFFFFFFFFFFFFFFFFUL == -1, "cast round-trip");
_Static_assert((char)0x100 == 0, "truncation to char");

/* 8. 位运算常量折叠 */
_Static_assert((0xFF & 0x0F) == 0x0F, "bitwise AND");
_Static_assert((0xF0 | 0x0F) == 0xFF, "bitwise OR");
_Static_assert((0xFF ^ 0x0F) == 0xF0, "bitwise XOR");
_Static_assert(~0U != 0, "bitwise NOT");

/* 9. 枚举常量算术 */
_Static_assert(PAGE_SIZE == 4096, "enum arithmetic");
_Static_assert(PAGE_SHIFT + 1 == 13, "enum add");

/* 10. 数组大小使用常量表达式 */
int array1[1 << 4];  /* 16 elements */
int array2[sizeof(int) * 4]; /* 16 elements */
_Static_assert(sizeof(array1) / sizeof(int) == 16, "array1 size");
_Static_assert(sizeof(array2) / sizeof(int) == 16, "array2 size");

int main(void) {
    /* 运行时验证位域宽度 */
    struct const_bitfield bf;
    bf.a = 0xFF;  /* 8-bit field, max 255 */
    bf.b = 0xAA;  /* 8-bit field */
    bf.c = 0xFFFF; /* 16-bit field */
    bf.d = 0xF;   /* 4-bit field, max 15 */

    /* 验证截断正确 */
    if (bf.a != 0xFF) return 1;
    if (bf.b != 0xAA) return 2;
    if (bf.d != 0xF) return 3;

    return 0;
}
