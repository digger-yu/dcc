// 测试 int128 减法结果
int main(void) {
    __int128 d = 100;
    __int128 e = 50;
    __int128 r = d - e;
    
    // 检查低 64 位
    long long lo = (long long)r;
    if (lo != 50) return 1;
    
    return 0;
}
