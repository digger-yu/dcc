// 测试 int128 初始化
int main(void) {
    __int128 a = 100;
    long long lo = (long long)a;
    if (lo != 100) return 1;
    return 0;
}
