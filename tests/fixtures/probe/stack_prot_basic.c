/* StackProtect 探针: 带局部字符数组的函数才会命中 -fstack-protector-strong */
int sp_copy(const char *src, char *dst, int n) {
    char buf[64];
    int i;
    for (i = 0; i < n; i++) buf[i] = src[i];
    for (i = 0; i < n; i++) dst[i] = buf[i];
    return n;
}
