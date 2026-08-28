// tests/fixtures/c/builtin_typedefs.c
// 编译器内置 typedef: size_t/ptrdiff_t/intptr_t/uintptr_t.
// 无需 #include <stddef.h>, 编译器直接提供这些类型.

size_t get_size(void) {
    return sizeof(int);
}

ptrdiff_t get_diff(const int *a, const int *b) {
    return a - b;
}

intptr_t ptr_to_int(void *p) {
    return (intptr_t)p;
}

uintptr_t ptr_to_uint(void *p) {
    return (uintptr_t)p;
}

// 验证类型大小正确 (LP64: 8 bytes)
_Static_assert(sizeof(size_t) == 8, "size_t should be 8 bytes on LP64");
_Static_assert(sizeof(ptrdiff_t) == 8, "ptrdiff_t should be 8 bytes on LP64");
_Static_assert(sizeof(intptr_t) == 8, "intptr_t should be 8 bytes on LP64");
_Static_assert(sizeof(uintptr_t) == 8, "uintptr_t should be 8 bytes on LP64");

// 验证有符号/无符号属性
_Static_assert((size_t)-1 > 1, "size_t should be unsigned");
_Static_assert((uintptr_t)-1 > 1, "uintptr_t should be unsigned");
_Static_assert((ptrdiff_t)-1 < 0, "ptrdiff_t should be signed");
_Static_assert((intptr_t)-1 < 0, "intptr_t should be signed");

int main(void) {
    size_t s = get_size();
    if (s != sizeof(int)) return 1;

    int arr[10];
    ptrdiff_t d = get_diff(&arr[5], &arr[2]);
    if (d != 3) return 2;

    intptr_t ip = ptr_to_int((void *)0x1234);
    if (ip != 0x1234) return 3;

    uintptr_t up = ptr_to_uint((void *)0x5678);
    if (up != 0x5678) return 4;

    return 0;
}
