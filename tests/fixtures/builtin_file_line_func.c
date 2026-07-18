/* builtin_file_line_func.c - 验证 __FILE__ / __LINE__ / __func__ 等 */

static int my_strcmp(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return (unsigned char)*a - (unsigned char)*b;
}

int my_func(int x) {
    int line_here = __builtin_LINE();
    const char *file_here = __builtin_FILE();
    const char *func_here = __builtin_FUNCTION();
    const char *pretty = __builtin_PRETTY_FUNCTION();
    const char *f1 = __func__;
    const char *f2 = __FUNCTION__;
    (void)x;
    if (func_here == 0 || my_strcmp(func_here, "my_func") != 0) return 1;
    if (pretty == 0 || my_strcmp(pretty, "my_func") != 0) return 2;
    if (f1 == 0 || my_strcmp(f1, "my_func") != 0) return 3;
    if (f2 == 0 || my_strcmp(f2, "my_func") != 0) return 4;
    if (file_here == 0) return 5;
    if (line_here <= 0) return 6;
    return 0;
}

int main(void) {
    return my_func(0);
}
