/* 报告 §5.52：局部结构体初始化器的**整块清零**与尺寸无关。
 *
 * C11 6.7.9p19/p21 要求初始化器未列出的成员（含填充字节）置零；旧实现只在
 * `struct_size <= 128` 时逐字节清零，更大的结构一个字节都不清。内核
 * `struct vma_merge_struct`（0x90B）的 `VMG_MMAP_STATE` 指定初始化器因此把栈
 * 残值留在 `.target`，`vma_expand()` 把 1 当 `vm_area_struct *` 解引用
 * （#PF at 0x29），boot 在 exec /init 之前 panic。
 *
 * 判据不依赖"栈上碰巧是垃圾"：`dirty_stack()` 先把即将被下一个调用帧复用的
 * 栈区填成 0xA5，再用 noinline 的检查函数做部分初始化 —— 修复前未列出字段
 * 必然是 0xA5，修复后必须是 0。逐位标记出错路径：
 *   1 144 B 结构        2 136 B 结构        3 128 B 边界（修复前就对）
 *   4 160 B union       5 嵌套（外层 152 B, 内层聚合字段部分初始化）
 */
typedef unsigned long u64;

struct big144 {
    u64 f0, f1, f2, f3, f4, f5, f6, f7, f8, f9;
    u64 g0, g1, g2, g3, g4, g5, g6, g7;
};

struct big136 {
    u64 f0, f1, f2, f3, f4, f5, f6, f7, f8, f9;
    u64 g0, g1, g2, g3, g4, g5;
};

struct edge128 {
    u64 f0, f1, f2, f3, f4, f5, f6, f7;
    u64 g0, g1, g2, g3, g4, g5, g6, g7;
};

union wide160 {
    u64 all[20]; /* 160 B */
    struct {
        u64 a, b;
    } s;
};

struct outer {
    struct big144 head;
    u64 tail;
};

static void __attribute__((noinline)) dirty_stack(void) {
    volatile unsigned char buf[600];
    for (int i = 0; i < 600; i++)
        buf[i] = 0xA5;
}

static int __attribute__((noinline)) check_big144(void) {
    struct big144 s = { .f0 = 7, .g7 = 9 };
    if (s.f0 != 7 || s.g7 != 9)
        return 1;
    if (s.f1 != 0 || s.f9 != 0 || s.g0 != 0 || s.g6 != 0)
        return 1;
    return 0;
}

static int __attribute__((noinline)) check_big136(void) {
    struct big136 s = { .f3 = 3 };
    if (s.f3 != 3)
        return 2;
    if (s.f0 != 0 || s.f4 != 0 || s.f9 != 0 || s.g5 != 0)
        return 2;
    return 0;
}

static int __attribute__((noinline)) check_edge128(void) {
    struct edge128 s = { .g7 = 5 };
    if (s.g7 != 5)
        return 4;
    if (s.f0 != 0 || s.g0 != 0 || s.g6 != 0)
        return 4;
    return 0;
}

static int __attribute__((noinline)) check_union160(void) {
    union wide160 u = { .s = { 5, 6 } };
    if (u.s.a != 5 || u.s.b != 6)
        return 8;
    for (int i = 2; i < 20; i++)
        if (u.all[i] != 0)
            return 8;
    return 0;
}

static int __attribute__((noinline)) check_nested(void) {
    struct outer o = { .head.g4 = 42, .tail = 43 };
    if (o.head.g4 != 42 || o.tail != 43)
        return 16;
    if (o.head.f0 != 0 || o.head.g3 != 0 || o.head.g5 != 0 || o.head.g7 != 0)
        return 16;
    return 0;
}

int main(void) {
    int rc = 0;

    dirty_stack();
    rc |= check_big144();
    dirty_stack();
    rc |= check_big136();
    dirty_stack();
    rc |= check_edge128();
    dirty_stack();
    rc |= check_union160();
    dirty_stack();
    rc |= check_nested();

    return rc;
}
