/* 测试: 结构体字段级 __attribute__((aligned(N))) */

/* 注意：本夹具在 gcc 下**故意不可编译** —— 下面的
 * `_Static_assert(sizeof(struct packed_aligned) == 5)` 记录的是 dcc 当前的
 * packed/字段 aligned 交互语义（packed 把对齐压到 1，得 5）；gcc 认为字段上的
 * `aligned(8)` 优先于结构体上的 `packed`，实测给 16（b 落在偏移 8）。除这一个
 * 结构外，其余八个布局两侧完全一致（basic=12 large=32 default=32 multi=16
 * ptr=32 array=24 outer=16）。差异已登记为待裁决项，不要为了“能差分”而把
 * 该断言改成两边都成立的不等式——那会抹掉唯一能抬出这个差异的地方。 */

/* 基本字段对齐: char 自然对齐 1, 但 aligned(4) 强制 4 字节对齐 */
struct basic {
    char a;
    int b __attribute__((aligned(4)));
    char c;
};

/* 验证偏移: a=0, b=4 (aligned(4)), c=8 */
_Static_assert(sizeof(struct basic) >= 12, "basic struct size");

/* 大对齐: 将 int 对齐到 16 字节 */
struct large_align {
    char x;
    int y __attribute__((aligned(16)));
};

/* x=0, y=16, total size >= 32 */
_Static_assert(sizeof(struct large_align) >= 20, "large align size");

/* aligned 无参数默认为 16 */
struct default_align {
    char a;
    int b __attribute__((aligned));
};

/* 与 packed 交互: packed 限制最大对齐为 1, aligned 不能突破 packed */
struct packed_aligned {
    char a;
    int b __attribute__((aligned(8)));
} __attribute__((packed));

/* packed 下 aligned 被限制: a=0, b=1 (packed 优先) */
_Static_assert(sizeof(struct packed_aligned) == 5, "packed overrides aligned");

/* 多位移字段对齐 */
struct multi_aligned {
    char a __attribute__((aligned(2)));
    char b __attribute__((aligned(4)));
    char c __attribute__((aligned(8)));
};

/* a=0, b=4, c=8, size >= 16 */
_Static_assert(sizeof(struct multi_aligned) >= 16, "multi aligned size");

/* 指针字段对齐 */
struct ptr_aligned {
    int x;
    int *p __attribute__((aligned(16)));
};

/* 数组字段对齐 */
struct array_aligned {
    char c;
    int arr[4] __attribute__((aligned(8)));
};

/* 嵌套结构体字段对齐 */
struct inner {
    char a;
    char b;
};

struct outer {
    char x;
    struct inner s __attribute__((aligned(8)));
};

/* 测试函数 */
int test_field_aligned(void) {
    struct basic b;
    b.a = 'A';
    b.b = 42;
    b.c = 'B';
    
    /* 验证对齐 */
    if (((unsigned long)&b.b & 3) != 0) return 1; /* b.b 应该 4 字节对齐 */
    
    struct large_align la;
    la.x = 1;
    la.y = 100;
    if (((unsigned long)&la.y & 15) != 0) return 2; /* la.y 应该 16 字节对齐 */
    
    return 0;
}

/* 入口：上面所有布局约束要么在 `_Static_assert`（编译期）里，要么在
 * `test_field_aligned` 的运行期地址检查里。没有 `main` 时这些运行期检查
 * 从来没被执行过。 */
int main(void) {
    return test_field_aligned();
}
