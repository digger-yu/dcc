/* va_copy 高级形式: 结构体成员 + 指针解引用 */
typedef __builtin_va_list va_list;

struct State {
    va_list ap;
    int count;
};

/* 通过结构体成员做 va_copy */
int sum_via_struct_member(int n, ...) {
    struct State st;
    __builtin_va_start(st.ap, n);
    st.count = n;

    struct State copy;
    __builtin_va_copy(copy.ap, st.ap);

    int s = 0;
    for (int i = 0; i < n; i++)
        s += __builtin_va_arg(copy.ap, int);
    __builtin_va_end(copy.ap);
    __builtin_va_end(st.ap);
    return s;
}

/* 通过指针解引用做 va_copy */
int sum_via_pointer(int n, ...) {
    va_list original;
    __builtin_va_start(original, n);

    va_list *pap = &original;
    va_list copy;
    __builtin_va_copy(copy, *pap);

    int s = 0;
    for (int i = 0; i < n; i++)
        s += __builtin_va_arg(copy, int);
    __builtin_va_end(copy);
    __builtin_va_end(original);
    return s;
}

int main(void) {
    /* sum_via_struct_member(4, 10, 20, 30, 40) = 100 */
    int r1 = sum_via_struct_member(4, 10, 20, 30, 40);
    if (r1 != 100) return 1;

    /* sum_via_pointer(3, 5, 15, 25) = 45 */
    int r2 = sum_via_pointer(3, 5, 15, 25);
    if (r2 != 45) return 2;

    return 0;
}
