// 严格类型检查: 隐式指针/整数转换 warning (M15)
extern int printf(const char *, ...);

int main(void) {
    long x = 42;
    // 隐式 int↔指针转换: 编译期可检测, 但非阻塞 (按 warning 类别报告)
    int *p = (int *)x;
    printf("%p\n", (void *)p);
    return 0;
}
