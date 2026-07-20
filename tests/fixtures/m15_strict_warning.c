// 严格类型检查: 隐式指针/整数转换 warning (M15)
int main(void) {
    long x = 42;
    // 隐式 int↔指针转换: 编译期可检测, 但非阻塞
    int *p = (int *)x;
    // 仅验证编译不阻塞 (无运行时校验)
    if (p) return 0;
    return 0;
}
