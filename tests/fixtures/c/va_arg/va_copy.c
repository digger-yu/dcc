/* va_copy 独立性验证: 复制后两个 va_list 应独立推进 */
typedef __builtin_va_list va_list;

int sum_pair(int n, ...) {
    va_list ap1, ap2;
    __builtin_va_start(ap1, n);
    __builtin_va_copy(ap2, ap1);
    /* ap1 和 ap2 应独立读取 */
    int s1 = 0, s2 = 0;
    for (int i = 0; i < n; i++)
        s1 += __builtin_va_arg(ap1, int);
    for (int i = 0; i < n; i++)
        s2 += __builtin_va_arg(ap2, int);
    __builtin_va_end(ap1);
    __builtin_va_end(ap2);
    /* s1 == s2, 返回 s1 + s2 */
    return s1 + s2;
}

int main(void) {
    /* sum_pair(3, 1, 2, 3) = 6 + 6 = 12 */
    return sum_pair(3, 1, 2, 3) - 12;
}
