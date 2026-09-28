/* 测试位域宽度使用常量表达式 */

/* 二元算术运算作为位域宽度 */
struct bitfield_arith {
    unsigned int a : (1 << 3);   /* 8 bits */
    unsigned int b : (4 + 4);    /* 8 bits */
    unsigned int c : (16 - 8);   /* 8 bits */
    unsigned int d : (2 * 4);    /* 8 bits */
};

/* sizeof 作为位域宽度 */
struct bitfield_sizeof {
    unsigned int x : (sizeof(int) * 2);  /* 64 bits on x86_64 */
    unsigned int y : (sizeof(char) * 8); /* 8 bits */
};

/* 枚举常量作为位域宽度 */
enum { BF_WIDTH = 5 };
struct bitfield_enum {
    unsigned int e : BF_WIDTH;   /* 5 bits */
    unsigned int f : (BF_WIDTH + 3); /* 8 bits */
};

/* 三元条件作为位域宽度 */
struct bitfield_cond {
    unsigned int g : (1 ? 8 : 4);  /* 8 bits */
};

/* 类型转换作为位域宽度 */
struct bitfield_cast {
    unsigned int h : ((unsigned int)(1 << 3)); /* 8 bits */
};

/* _Static_assert 验证位域宽度正确 */
_Static_assert(sizeof(struct bitfield_arith) == 4, "bitfield_arith size");
_Static_assert(sizeof(struct bitfield_enum) == 4, "bitfield_enum size");

int main(void) {
    struct bitfield_arith ba;
    ba.a = 0xFF;
    ba.b = 0xAA;
    ba.c = 0x55;
    ba.d = 0x12;
    return ba.a + ba.b + ba.c + ba.d;
}
