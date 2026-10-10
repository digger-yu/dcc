// 测试: bar(10) + bar(10) (期望 20)
int bar(int x) {
    if ((x)) return x;
    return 0;
}
int main(void) {
    int r = 0;
    r += bar(10);
    r += bar(10);
    return r;
}
